//! p5z mode against a real `.p5z` that grew by appending, made by the
//! MaxPack engine (`tests/fixtures/p5z-grown/README.md`): chunks out of
//! record order, room in the old volumes, a volume with no chunk and volume
//! headers written before it grew. It is mounted as the PS5 mounts it in p5z
//! mode (the start-up check, then `Index::open_p5z` with the tables block,
//! each volume a window of the one `.p5z`), and every file its index serves
//! is read back, whole and in ranges, against the SHA-256 the engine wrote.
use ampr_codec::{ZstdDecoder, ZSTD_DECODER_WORKSPACE, ZSTD_MAX_CHUNK_SIZE};
use ampr_format::p5z::{check_header, check_region, file_offset, parse_image, Region, MAX_VOLUMES};
use ampr_format::zstd::{
    header_counts, header_volumes, volume_header, volume_header_for, ChunkRecord, FileRecord,
    Index, P5zOpenError, Rules, CHUNK_SHIFT, CHUNK_SIZE, INDEX_HEADER_SIZE, VOLUME_HEADER_SIZE,
};
use ampr_format::Error;
use std::collections::BTreeSet;

mod sha256;
use sha256::sha256_hex;

const P5Z: &[u8] = include_bytes!("fixtures/p5z-grown/grown.p5z");
const INDEX: &[u8] = include_bytes!("fixtures/p5z-grown/ampr_zstd.index");
const IMAGE: &[u8] = include_bytes!("fixtures/p5z-grown/maxpack.image");
/// Every file `ampr_zstd.index` serves: `<sha256> <size> <path>` per line.
const FILES: &str = include_str!("fixtures/p5z-grown/files.txt");

/// Per volume, as the README lists them: the volume count in its header
/// (the volumes the `.p5z` had when it was written), its chunks and the
/// bytes no chunk uses.
const HEADER_COUNTS: [u32; 5] = [3, 3, 3, 4, 5];
const CHUNKS: [u32; 5] = [39, 0, 4, 8, 3];
const UNUSED: [u64; 5] = [92_013, 262_144, 137_856, 0, 0];

// ---- the mount ----

/// The volume regions of `maxpack.image`.
fn regions() -> Vec<Region> {
    let mut out = [Region::default(); MAX_VOLUMES];
    let (path, n) = parse_image(IMAGE, &mut out).unwrap();
    assert_eq!(path, b"p5z/grown.p5z");
    out[..n].to_vec()
}

/// The 64-byte volume header at the start of region `r`.
fn header(r: Region) -> &'static [u8] {
    &P5Z[r.start as usize..r.start as usize + VOLUME_HEADER_SIZE]
}

/// The block for the chunk and volume tables, sized from the index header.
fn tables(index: &[u8]) -> Vec<u64> {
    let (_, chunks, volumes) = header_counts(index).unwrap();
    vec![0; (chunks + u64::from(volumes)) as usize]
}

/// `index` mounted as p5z mode mounts it (ampr-sys `rt/pack/zstd.rs`): the
/// chunk part of the tables block holds the sorted copy in which
/// `open_p5z` checks where the chunks sit; then the whole block is filled
/// with the tables in chunk order, which `chunk` and `volume_size` read.
fn mount<'t>(index: &'t [u8], tables: &'t mut [u64]) -> Result<Index<'t>, P5zOpenError> {
    let (_, chunks, _) = header_counts(index)?;
    let (c, v) = tables.split_at_mut(chunks as usize);
    let opened = Index::open_p5z(index, c)?;
    opened.fill_chunks(c, v)?;
    Ok(opened.with_chunks(c, v)?)
}

/// What the PS5 checks at start-up, before `ampr_zstd.index` is mounted
/// (docs/p5z-mode.md, steps 1 to 6). The headers of the volumes written
/// before the `.p5z` grew count fewer volumes than the index: p5z rules
/// take them, pack rules take only the last one.
#[test]
fn grown_p5z_passes_the_start_up_check() {
    let regions = regions();
    let v = header_volumes(&INDEX[..INDEX_HEADER_SIZE]).unwrap();
    assert_eq!(v.count as usize, regions.len());
    check_header(&P5Z[..64], &v.archive_id).unwrap();
    for (k, &r) in (0u32..).zip(&regions) {
        let at = v.volumes_at as usize + 8 * k as usize;
        let size = u64::from_le_bytes(INDEX[at..at + 8].try_into().unwrap());
        check_region(k, r, P5Z.len() as u64, size).unwrap();
        let h = header(r);
        let taken = volume_header_for(h, k, v.count, &v.archive_id, size, Rules::P5z).unwrap();
        assert_eq!(taken.count, HEADER_COUNTS[k as usize], "volume {k}");
        let pack = volume_header_for(h, k, v.count, &v.archive_id, size, Rules::Pack);
        assert_eq!(pack.is_ok(), taken.count == v.count, "volume {k}");
    }
}

/// The mount takes the grown index with p5z rules, and the index is what
/// the README says: chunks out of record order, room in the old volumes, a
/// volume with no chunk. Each volume's first open checks its region and
/// header again against the mounted index.
#[test]
fn grown_index_mounts_with_p5z_rules() {
    let mut block = tables(INDEX);
    let index = mount(INDEX, &mut block).unwrap();
    assert!(index.has_chunks());
    assert_eq!(index.rules(), Rules::P5z);
    let regions = regions();
    assert_eq!(index.volume_count() as usize, regions.len());
    assert_eq!((index.file_count(), index.chunk_count()), (45, 54));
    check_header(&P5Z[..64], index.archive_id()).unwrap();

    let (mut chunks, mut used) = ([0u32; 5], [0u64; 5]);
    let (mut in_order, mut last) = (true, (0, 0));
    for id in 0..index.chunk_count() {
        let c = index.chunk(id).unwrap();
        let v = usize::from(c.volume_id);
        chunks[v] += 1;
        used[v] += u64::from(c.stored_size);
        in_order &= (c.volume_id, c.offset) >= last;
        last = (c.volume_id, c.offset);
    }
    assert!(!in_order, "the chunks are in record order: not a grown index");
    assert_eq!(chunks, CHUNKS);
    for (k, &r) in (0u32..).zip(&regions) {
        let size = index.volume_size(k).unwrap();
        check_region(k, r, P5Z.len() as u64, size).unwrap();
        let h = volume_header(header(r), &index, k).unwrap();
        assert_eq!(h.count, HEADER_COUNTS[k as usize], "volume {k}");
        assert_eq!(size - VOLUME_HEADER_SIZE as u64 - used[k as usize], UNUSED[k as usize]);
    }
}

/// Outside p5z mode the rules of every zstd pack are unchanged: the grown
/// index is refused (its chunks do not fill the volumes in record order),
/// and so are the volume headers written before it grew.
#[test]
fn grown_index_is_refused_outside_p5z_mode() {
    assert_eq!(Index::open(INDEX).err(), Some(Error::Invalid));
    let mut block = tables(INDEX);
    let index = mount(INDEX, &mut block).unwrap();
    let (count, id) = (index.volume_count(), index.archive_id());
    for (k, &r) in (0u32..).zip(&regions()) {
        let size = index.volume_size(k).unwrap();
        let pack = volume_header_for(header(r), k, count, id, size, Rules::Pack);
        assert_eq!(pack.is_ok(), k + 1 == count, "volume {k}");
    }
}

// ---- reading the files back ----

/// One line of `files.txt`.
struct Listed {
    sha256: &'static str,
    size: u64,
    path: &'static str,
}

fn listed() -> Vec<Listed> {
    FILES
        .lines()
        .map(|line| {
            let mut f = line.splitn(3, ' ');
            let (sha256, size, path) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
            assert_eq!(sha256.len(), 64, "{line}");
            Listed {
                sha256,
                size: size.parse().unwrap(),
                path,
            }
        })
        .collect()
}

#[repr(C, align(64))]
struct Workspace([u8; ZSTD_DECODER_WORKSPACE]);

/// Reads packed files as p5z mode reads them: a task for each 256 KiB chunk
/// a range touches, as ampr-sys `zstd_read_task` makes them; the chunk's
/// stored bytes come from its volume's window of the one `.p5z`
/// (`file_offset`, the region start added once) and go through the PS5's
/// chunk decoder.
struct Reader<'a> {
    index: &'a Index<'a>,
    regions: &'a [Region],
    decoder: &'a mut ZstdDecoder,
    scratch: &'a mut [u8],
    /// How many times each chunk was decoded.
    decoded: Vec<u32>,
    /// Reads that touched two chunks or more.
    spanning: u32,
}

impl Reader<'_> {
    fn read(&mut self, file: &FileRecord, offset: u64, len: u64) -> Vec<u8> {
        assert!(len > 0 && offset <= file.logical_size && len <= file.logical_size - offset);
        let end = offset + len;
        let (first, last) = (offset >> CHUNK_SHIFT, (end - 1) >> CHUNK_SHIFT);
        self.spanning += u32::from(last > first);
        let mut out = vec![0u8; len as usize];
        for local in first..=last {
            assert!(local < u64::from(file.chunk_count));
            let id = file.first_chunk + local as u32;
            let c: ChunkRecord = self.index.chunk(id).unwrap();
            let begin = local << CHUNK_SHIFT;
            let raw = (file.logical_size - begin).min(CHUNK_SIZE);
            let r = self.regions[usize::from(c.volume_id)];
            let at = file_offset(r, u64::from(c.offset)).unwrap() as usize;
            let stored_end = u64::from(c.offset) + u64::from(c.stored_size);
            assert!(stored_end <= r.len, "chunk {id} past its region");
            let stored = &P5Z[at..at + c.stored_size as usize];
            self.decoder
                .decode(c.codec, raw as usize, stored, self.scratch)
                .unwrap_or_else(|e| panic!("chunk {id}: {e:?}"));
            self.decoded[id as usize] += 1;
            let (from, to) = (offset.max(begin), end.min(begin + raw));
            out[(from - offset) as usize..(to - offset) as usize]
                .copy_from_slice(&self.scratch[(from - begin) as usize..(to - begin) as usize]);
        }
        out
    }
}

/// Odd-sized ranges of a file of `size` bytes (at least 1): its first and
/// last byte, its middle third, all of it but the first and last byte,
/// ranges of a few odd sizes across every 256 KiB chunk boundary, and one
/// across three chunks.
fn ranges(size: u64) -> Vec<(u64, u64)> {
    let mut out = vec![(0, 1), (size - 1, 1), (size / 3, (size / 3).max(1))];
    if size > 2 {
        out.push((1, size - 2));
    }
    let around = [
        (1, 1),
        (3, 4),
        (4_095, 4_098),
        (100_003, 77_777),
        (CHUNK_SIZE - 1, 2),
    ];
    let mut boundary = CHUNK_SIZE;
    while boundary < size {
        for (before, after) in around {
            let start = boundary - before;
            let end = (boundary + after).min(size);
            out.push((start, end - start));
        }
        boundary += CHUNK_SIZE;
    }
    if size > 2 * CHUNK_SIZE {
        out.push((CHUNK_SIZE - 1, CHUNK_SIZE + 2));
    }
    out
}

/// Every file the grown index serves, and nothing else, is in `files.txt`
/// with the size the index gives it; read whole, its bytes have the
/// SHA-256 the engine wrote, and every range, and every pass through it in
/// odd-sized reads, gives those same bytes. Every chunk is decoded.
#[test]
fn every_file_of_the_grown_index_reads_back_whole_and_in_ranges() {
    let mut block = tables(INDEX);
    let index = mount(INDEX, &mut block).unwrap();
    let regions = regions();
    let files = listed();

    let mut paths: BTreeSet<&str> = files.iter().map(|f| f.path).collect();
    assert_eq!(paths.len(), files.len(), "a path listed twice");
    assert_eq!(index.file_count(), files.len() as u64);
    let mut buf = [0u8; 1024];
    for id in 0..index.file_count() {
        let rec = index.file(id).unwrap();
        let path = index.path(&rec, &mut buf).unwrap();
        let path = std::str::from_utf8(path).unwrap();
        assert!(paths.remove(path), "{path} is not in files.txt");
    }

    let mut ws = Box::new(Workspace([0; ZSTD_DECODER_WORKSPACE]));
    let mut scratch = vec![0u8; ZSTD_MAX_CHUNK_SIZE];
    // SAFETY: `ws` is writable for its whole length, 64 B aligned, used for
    // nothing else, and outlives the decoder, whose last use is in this
    // function.
    let decoder = unsafe { ZstdDecoder::init_in(ws.0.as_mut_ptr(), ZSTD_DECODER_WORKSPACE) };
    let decoder = unsafe { decoder.as_mut() }.expect("the decoder");
    decoder.warm_up(&mut scratch).unwrap();
    let mut reader = Reader {
        index: &index,
        regions: &regions,
        decoder,
        scratch: &mut scratch,
        decoded: vec![0; index.chunk_count() as usize],
        spanning: 0,
    };
    for entry in &files {
        let name = entry.path;
        let id = index.find(name.as_bytes()).unwrap().expect(name);
        let file = index.file(id).unwrap();
        assert_eq!(file.logical_size, entry.size, "{name}");
        assert!(entry.size > 0, "{name}: an empty file has no chunk to read");
        let whole = reader.read(&file, 0, entry.size);
        assert_eq!(sha256_hex(&whole), entry.sha256, "{name}");
        for (offset, len) in ranges(entry.size) {
            let got = reader.read(&file, offset, len);
            let want = &whole[offset as usize..(offset + len) as usize];
            assert!(got == want, "{name}: {len} bytes at {offset}");
        }
        for step in [65_537, 300_007] {
            let mut again = Vec::with_capacity(whole.len());
            let mut at = 0;
            while at < entry.size {
                let len = step.min(entry.size - at);
                again.extend(reader.read(&file, at, len));
                at += len;
            }
            assert!(again == whole, "{name}: in reads of {step} bytes");
        }
    }
    let never: Vec<usize> = (0..reader.decoded.len())
        .filter(|&id| reader.decoded[id] == 0)
        .collect();
    assert!(never.is_empty(), "chunks never decoded: {never:?}");
    assert!(reader.spanning >= 50, "{} reads across chunks", reader.spanning);
}

#[test]
fn sha256_known_answers() {
    for (data, want) in [
        (&b""[..], "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        assert_eq!(sha256_hex(data), want);
    }
    assert_eq!(
        sha256_hex(&vec![b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

// ---- broken copies of the grown index ----

/// The chunk record of chunk `n` of file `path`, and its id.
fn chunk_of(index: &Index<'_>, path: &str, n: u32) -> (u32, ChunkRecord) {
    let id = index.find(path.as_bytes()).unwrap().unwrap();
    let file = index.file(id).unwrap();
    assert!(n < file.chunk_count, "{path}");
    let id = file.first_chunk + n;
    (id, index.chunk(id).unwrap())
}

/// `INDEX` with chunk record `id` moved to `offset` of `volume`, its
/// payload CRC sealed again.
fn moved(id: u32, volume: u16, offset: u32) -> Vec<u8> {
    let mut out = INDEX.to_vec();
    let chunks_at = u64::from_le_bytes(out[64..72].try_into().unwrap()) as usize;
    let at = chunks_at + 8 * id as usize;
    let c = u64::from_le_bytes(out[at..at + 8].try_into().unwrap());
    // Bits 0..32 the offset, 32..44 the volume: the size and codec stay.
    let c = (c & !((1 << 44) - 1)) | (u64::from(volume) << 32) | u64::from(offset);
    out[at..at + 8].copy_from_slice(&c.to_le_bytes());
    let crc = ampr_format::crc32(&out[INDEX_HEADER_SIZE..]);
    out[112..116].copy_from_slice(&crc.to_le_bytes());
    out
}

/// Copies of the grown index with one chunk moved, as a wrong append could
/// leave them. Two chunks that overlap, in an old volume or an appended
/// one, or a chunk that runs past its volume's end, the empty one too, are
/// refused by p5z rules, each naming its volume; a chunk moved into room no
/// chunk uses is taken. Pack rules refuse every copy, as the original.
#[test]
fn broken_copies_of_the_grown_index_are_refused() {
    let mut block = tables(INDEX);
    let index = mount(INDEX, &mut block).unwrap();
    let size = |v: u32| index.volume_size(v).unwrap() as u32;
    let (s01, c01) = chunk_of(&index, "small/s01.txt", 0);
    let (s02, c02) = chunk_of(&index, "small/s02.txt", 0);
    let (_, c11) = chunk_of(&index, "small/s11.txt", 0);
    let (s15, _) = chunk_of(&index, "small/s15.txt", 0);
    let (s16, c16) = chunk_of(&index, "small/s16.txt", 0);
    let (_, c38) = chunk_of(&index, "small/s38.txt", 0);
    let (big1, cbig1) = chunk_of(&index, "data/big.bin", 1);
    let (dlc2, cdlc2) = chunk_of(&index, "addcont0/data/sub/dlc2.bin", 0);
    // Where the README puts them: small/s38.txt's chunk ends the room in
    // volume 0, small/s16.txt's and dlc2.bin's end volumes 2 and 4, and the
    // update wrote big.bin to volume 3.
    assert_eq!((c02.volume_id, c38.volume_id, c11.volume_id), (0, 0, 2));
    assert_eq!(c16.offset + c16.stored_size, size(2));
    assert_eq!(cbig1.volume_id, 3);
    assert_eq!((cdlc2.volume_id, cdlc2.offset + cdlc2.stored_size), (4, size(4)));
    let room_end = c38.offset;
    use P5zOpenError::{Overlap, PastEnd};
    let cases: [(&str, Vec<u8>, Result<(), P5zOpenError>); 11] = [
        ("unchanged", moved(s01, 0, c01.offset), Ok(())),
        (
            "into the room an update left, up to the next chunk",
            moved(s01, 0, room_end - c01.stored_size),
            Ok(()),
        ),
        (
            "into the volume with no chunk",
            moved(s16, 1, 64),
            Ok(()),
        ),
        (
            "one byte onto the chunk before it, in an old volume",
            moved(s02, 0, c02.offset - 1),
            Err(Overlap { volume: 0 }),
        ),
        (
            "one byte onto the chunk before it, in an appended volume",
            moved(big1, 3, cbig1.offset - 1),
            Err(Overlap { volume: 3 }),
        ),
        (
            "at the offset of another chunk",
            moved(s15, 2, c11.offset),
            Err(Overlap { volume: 2 }),
        ),
        (
            "into the room an update left, one byte onto the next chunk",
            moved(s01, 0, room_end - c01.stored_size + 1),
            Err(Overlap { volume: 0 }),
        ),
        (
            "one byte past the end of an old volume",
            moved(s16, 2, c16.offset + 1),
            Err(PastEnd { volume: 2 }),
        ),
        (
            "one byte past the end of the last appended volume",
            moved(dlc2, 4, cdlc2.offset + 1),
            Err(PastEnd { volume: 4 }),
        ),
        (
            "past the end of the volume with no chunk",
            moved(s16, 1, size(1) - c16.stored_size + 1),
            Err(PastEnd { volume: 1 }),
        ),
        (
            "starting where an old volume ends",
            moved(s16, 2, size(2)),
            Err(PastEnd { volume: 2 }),
        ),
    ];
    for (what, bytes, p5z) in cases {
        assert_eq!(Index::open(&bytes[..]).err(), Some(Error::Invalid), "{what}");
        let mut block = tables(&bytes);
        assert_eq!(mount(&bytes, &mut block).map(|_| ()), p5z, "{what}");
    }
}
