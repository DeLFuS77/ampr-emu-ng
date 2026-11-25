//! Readers for zstd pack indexes (AMPRZPK1 v1) and volumes (AMPRZDT1 v1).
//! The convention (magics, layout, names, chunk records, the volume header) comes
//! from the `ampr-zstd` crate, used in `no_std`; the index reader reads through a
//! `Source`, so the runtime reads a large index at offsets instead of holding
//! it whole.
use crate::source::{src_bytes, src_crc32, src_u64, Source, PATH_BUF};
use crate::{crc32_parts, u16_at, u32_at, u64_at, Error, Result};
use ampr_zstd as format;
use core::cmp::Ordering;

pub use format::{
    path_hash_bytes as path_hash, VolumeHeader, CHUNK_RECORD_SIZE, CHUNK_SHIFT, CHUNK_SIZE,
    FILE_RECORD_SIZE, FORMAT_VERSION, INDEX_HEADER_CRC_AT, INDEX_HEADER_SIZE, INDEX_MAGIC,
    INDEX_NAME, INDEX_PAYLOAD_CRC_AT, MAX_VOLUMES, VOLUME_HEADER_SIZE, VOLUME_LIMIT, VOLUME_PREFIX,
    VOLUME_RECORD_SIZE, VOLUME_SUFFIX, ZSTD_LEVELS,
};

/// A file record by value: the path stays in the source at `path_at`
/// (absolute offset) and is copied out with `Index::path`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileRecord {
    pub path_at: u64,
    pub path_len: u16,
    pub path_hash: u64,
    pub logical_size: u64,
    pub first_chunk: u32,
    pub chunk_count: u32,
    pub store_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkRecord {
    pub offset: u32,
    pub volume_id: u16,
    pub stored_size: u32,
    pub codec: u8,
}

/// The rules an index was opened with, which its volume headers are
/// checked by too (`volume_header`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rules {
    /// `Index::open`, every zstd pack: the chunks fill each volume from
    /// byte 64 in record order (files in record order, chunks in file
    /// order), back to back, and each volume header counts the index's
    /// volumes.
    Pack,
    /// `Index::open_p5z`, p5z mode (docs/p5z-mode.md), whose `.p5z`
    /// MaxPack can grow by appending: the chunks of each volume sit in
    /// [64, size) in any order, with room between them allowed and no
    /// overlap, and a volume header may count fewer volumes than the
    /// index.
    P5z,
}

/// Why `Index::open_p5z` refused an index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum P5zOpenError {
    /// A check every index makes (`Index::open`'s error).
    Index(Error),
    /// Two chunks of `volume` overlap.
    Overlap { volume: u32 },
    /// A chunk of `volume` runs past its end.
    PastEnd { volume: u32 },
}

impl From<Error> for P5zOpenError {
    fn from(e: Error) -> Self {
        Self::Index(e)
    }
}

/// Reads records through `src` at absolute table offsets, so the same
/// reader serves a resident image and the runtime's file sources, which
/// read the index file at offsets.
pub struct Index<'s, S: Source + ?Sized = [u8]> {
    src: &'s S,
    rules: Rules,
    /// Resident chunk and volume tables, set by `with_chunks`; file
    /// records and paths stay in the source.
    tables: Option<(&'s [u64], &'s [u64])>,
    files_at: u64,
    file_count: u64,
    chunks_at: u64,
    chunk_count: u32,
    volumes_at: u64,
    volume_count: u32,
    strings_at: u64,
    strings_len: u64,
    archive_id: [u8; 16],
    total_logical_bytes: u64,
}

/// The 128-byte index header. Its checks are the ones that need no other
/// byte of the index, so `Index::open` (the mount) and `header_volumes`
/// (the p5z start-up check) refuse the same headers.
struct Header {
    volume_count: u32,
    file_count: u64,
    chunk_count: u64,
    files_off: u64,
    chunks_off: u64,
    volumes_off: u64,
    strings_off: u64,
    strings_len: u64,
    archive_id: [u8; 16],
    total_logical_bytes: u64,
}

impl Header {
    /// The checks `Index::open` makes before the payload CRC: magic,
    /// version, header size, chunk shift, zstd level and reserved fields
    /// (Invalid), then the header CRC (Checksum).
    fn read(h: &[u8; INDEX_HEADER_SIZE]) -> Result<Self> {
        if &h[..8] != INDEX_MAGIC
            || u32_at(h, 8)? != FORMAT_VERSION
            || u32_at(h, 12)? != INDEX_HEADER_SIZE as u32
            || h[16] != CHUNK_SHIFT
            || !ZSTD_LEVELS.contains(&h[17])
            || u16_at(h, 18)? != 0
            || u64_at(h, 104)? != 0
            || u64_at(h, 120)? != 0
        {
            return Err(Error::Invalid);
        }
        let crc_end = INDEX_HEADER_CRC_AT + 4;
        if crc32_parts(&[&h[..INDEX_PAYLOAD_CRC_AT], &[0; 8], &h[crc_end..]])
            != u32_at(h, INDEX_HEADER_CRC_AT)?
        {
            return Err(Error::Checksum);
        }
        let mut archive_id = [0; 16];
        archive_id.copy_from_slice(&h[24..40]);
        Ok(Self {
            volume_count: u32_at(h, 20)?,
            file_count: u64_at(h, 40)?,
            chunk_count: u64_at(h, 48)?,
            files_off: u64_at(h, 56)?,
            chunks_off: u64_at(h, 64)?,
            volumes_off: u64_at(h, 72)?,
            strings_off: u64_at(h, 80)?,
            strings_len: u64_at(h, 88)?,
            archive_id,
            total_logical_bytes: u64_at(h, 96)?,
        })
    }

    /// The checks `Index::open` makes after the payload CRC: the counts and
    /// the table layout (Invalid). `file_len` is the index's length; None
    /// (the header alone) skips the one rule that needs it, that the
    /// strings end the file.
    fn check(&self, file_len: Option<u64>) -> Result<()> {
        if self.volume_count as usize > MAX_VOLUMES || self.chunk_count > u32::MAX as u64 {
            return Err(Error::Invalid);
        }
        let strings_end = match (self.strings_off.checked_add(self.strings_len), file_len) {
            (None, _) => false,
            (Some(end), Some(len)) => end == len,
            (Some(_), None) => true,
        };
        // chunk_count fits u32 (above), so `* CHUNK_RECORD_SIZE` cannot wrap.
        if self.files_off != INDEX_HEADER_SIZE as u64
            || self.files_off.checked_add(
                self.file_count
                    .checked_mul(FILE_RECORD_SIZE as u64)
                    .ok_or(Error::Invalid)?,
            ) != Some(self.chunks_off)
            || self
                .chunks_off
                .checked_add(self.chunk_count * CHUNK_RECORD_SIZE as u64)
                != Some(self.volumes_off)
            || self
                .volumes_off
                .checked_add(u64::from(self.volume_count) * VOLUME_RECORD_SIZE as u64)
                != Some(self.strings_off)
            || !strings_end
            || self.strings_len > u32::MAX as u64
            || (self.chunk_count == 0) != (self.volume_count == 0)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

impl<'s, S: Source + ?Sized> Index<'s, S> {
    /// Opens and checks the whole index under `Rules::Pack`.
    pub fn open(src: &'s S) -> Result<Self> {
        Self::open_with(src, None)
    }

    /// `open` under `Rules::P5z`: the chunks may sit in their volumes in
    /// any order, with room between them, as long as no two overlap and
    /// each one stays inside its volume. `sorted` holds one u64 per chunk
    /// (the header's chunk count, else `Index(Invalid)`) for a sorted copy
    /// of where they sit; what it holds afterwards is unspecified.
    pub fn open_p5z(
        src: &'s S,
        sorted: &mut [u64],
    ) -> core::result::Result<Self, P5zOpenError> {
        let index = Self::open_with(src, Some(&mut *sorted))?;
        index.check_placement(sorted)?;
        Ok(index)
    }

    fn open_with(src: &'s S, sorted: Option<&mut [u64]>) -> Result<Self> {
        let mut h = [0u8; INDEX_HEADER_SIZE];
        src.read_at(0, &mut h)?;
        let header = Header::read(&h)?;
        if src_crc32(src, INDEX_HEADER_SIZE as u64, src.len(), 0)?
            != u32_at(&h, INDEX_PAYLOAD_CRC_AT)?
        {
            return Err(Error::Checksum);
        }
        header.check(Some(src.len()))?;
        let index = Self {
            src,
            rules: if sorted.is_some() {
                Rules::P5z
            } else {
                Rules::Pack
            },
            tables: None,
            files_at: header.files_off,
            file_count: header.file_count,
            chunks_at: header.chunks_off,
            // `check` bounds it by u32::MAX.
            chunk_count: header.chunk_count as u32,
            volumes_at: header.volumes_off,
            volume_count: header.volume_count,
            strings_at: header.strings_off,
            strings_len: header.strings_len,
            archive_id: header.archive_id,
            total_logical_bytes: header.total_logical_bytes,
        };
        index.validate(sorted)?;
        Ok(index)
    }

    pub fn rules(&self) -> Rules {
        self.rules
    }
    pub fn archive_id(&self) -> &[u8; 16] {
        &self.archive_id
    }
    pub fn file_count(&self) -> u64 {
        self.file_count
    }
    pub fn chunk_count(&self) -> u32 {
        self.chunk_count
    }
    pub fn volume_count(&self) -> u32 {
        self.volume_count
    }

    // ---- chunk tables ----

    /// The chunk and volume tables in one sequential pass, through `buf`.
    fn read_chunks(&self, buf: &mut [u8], chunks: &mut [u64], volumes: &mut [u64]) -> Result<()> {
        let step = buf.len() / 8;
        for (table, out) in [(self.chunks_at, chunks), (self.volumes_at, volumes)] {
            for (n, out) in out.chunks_mut(step).enumerate() {
                let block = &mut buf[..out.len() * 8];
                self.src.read_at(table + (n * step * 8) as u64, block)?;
                for (i, v) in out.iter_mut().enumerate() {
                    *v = u64_at(block, i * 8)?;
                }
            }
        }
        Ok(())
    }

    /// Lengths of the chunk-only form's arrays: (chunks, volumes), 8 B
    /// each. It has no per-file part: a caller that keeps its own file
    /// projection holds only these.
    pub fn chunk_lens(&self) -> (usize, usize) {
        (self.chunk_count as usize, self.volume_count as usize)
    }

    /// Fills the chunk and volume tables in one sequential pass, 4 KiB per
    /// source read (no_std threads have small stacks). `Invalid` when a
    /// slice length differs from `chunk_lens`.
    pub fn fill_chunks(&self, chunks: &mut [u64], volumes: &mut [u64]) -> Result<()> {
        if (chunks.len(), volumes.len()) != self.chunk_lens() {
            return Err(Error::Invalid);
        }
        self.read_chunks(&mut [0u8; 4096], chunks, volumes)
    }

    /// Makes `chunk` and `volume_size` read `chunks` and `volumes` (filled
    /// by `fill_chunks` from this index); `file`, `path`,
    /// `logical_size` and `find` still read the source. `Invalid` when the
    /// lengths do not match.
    pub fn with_chunks(mut self, chunks: &'s [u64], volumes: &'s [u64]) -> Result<Self> {
        if (chunks.len(), volumes.len()) != self.chunk_lens() {
            return Err(Error::Invalid);
        }
        self.tables = Some((chunks, volumes));
        Ok(self)
    }

    /// Whether `chunk` and `volume_size` read memory only (`with_chunks`).
    pub fn has_chunks(&self) -> bool {
        self.tables.is_some()
    }

    fn raw_file(&self, id: u64) -> Result<[u8; FILE_RECORD_SIZE]> {
        if id >= self.file_count {
            return Err(Error::OutOfRange);
        }
        let mut r = [0u8; FILE_RECORD_SIZE];
        self.src
            .read_at(self.files_at + id * FILE_RECORD_SIZE as u64, &mut r)?;
        Ok(r)
    }

    fn parse_file(&self, r: &[u8]) -> Result<FileRecord> {
        let path_off = u64::from(u32_at(r, 20)?);
        let path_len = u16_at(r, 24)?;
        if path_off + u64::from(path_len) > self.strings_len {
            return Err(Error::Invalid);
        }
        let size = u64_at(r, 8)?;
        let chunk_count = size / CHUNK_SIZE + u64::from(size % CHUNK_SIZE != 0);
        Ok(FileRecord {
            path_at: self.strings_at + path_off,
            path_len,
            path_hash: u64_at(r, 0)?,
            logical_size: size,
            first_chunk: u32_at(r, 16)?,
            chunk_count: u32::try_from(chunk_count).map_err(|_| Error::Invalid)?,
            store_only: u16_at(r, 26)? & 1 != 0,
        })
    }

    pub fn file(&self, id: u64) -> Result<FileRecord> {
        self.parse_file(&self.raw_file(id)?)
    }

    /// File `id`'s logical size, from its record in the source.
    pub fn logical_size(&self, id: u64) -> Result<u64> {
        Ok(self.file(id)?.logical_size)
    }

    /// Copies `rec`'s path into `buf`; `Invalid` when it does not fit.
    pub fn path<'b>(&self, rec: &FileRecord, buf: &'b mut [u8]) -> Result<&'b [u8]> {
        src_bytes(self.src, rec.path_at, usize::from(rec.path_len), buf)
    }

    pub fn chunk(&self, id: u32) -> Result<ChunkRecord> {
        if id >= self.chunk_count {
            return Err(Error::OutOfRange);
        }
        let packed = match self.tables {
            Some((chunks, _)) => chunks[id as usize],
            None => src_u64(
                self.src,
                self.chunks_at + u64::from(id) * CHUNK_RECORD_SIZE as u64,
            )?,
        };
        // The format crate's record layout; a record it refuses (codec 2 or 3,
        // or out of a volume's bounds) is invalid.
        let chunk = format::Chunk::unpack(packed).map_err(|_| Error::Invalid)?;
        Ok(ChunkRecord {
            offset: chunk.offset,
            volume_id: chunk.volume_id,
            stored_size: chunk.stored_size,
            codec: match chunk.codec {
                format::Codec::Raw => 0,
                format::Codec::Zstd => 1,
            },
        })
    }

    pub fn volume_size(&self, id: u32) -> Result<u64> {
        if id >= self.volume_count {
            return Err(Error::OutOfRange);
        }
        match self.tables {
            Some((_, volumes)) => Ok(volumes[id as usize]),
            None => src_u64(
                self.src,
                self.volumes_at + u64::from(id) * VOLUME_RECORD_SIZE as u64,
            ),
        }
    }

    /// Zero-based file id in sorted (hash, path) order. The binary search
    /// compares hashes and reads a record's path only on an equal hash.
    pub fn find(&self, path: &[u8]) -> Result<Option<u64>> {
        if !valid_path(path) {
            return Err(Error::Invalid);
        }
        if path.len() >= PATH_BUF {
            return Ok(None);
        }
        let hash = path_hash(path);
        let mut buf = [0u8; PATH_BUF];
        let (mut low, mut high) = (0u64, self.file_count);
        while low < high {
            let mid = low + (high - low) / 2;
            let record = self.file(mid)?;
            let less = match record.path_hash.cmp(&hash) {
                Ordering::Equal => self.path(&record, &mut buf)? < path,
                o => o == Ordering::Less,
            };
            if less {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if low >= self.file_count {
            return Ok(None);
        }
        let record = self.file(low)?;
        Ok((record.path_hash == hash && self.path(&record, &mut buf)? == path).then_some(low))
    }

    /// One pass over the records through the source. Two path buffers hold
    /// the current and the previous path for the order check. Where the
    /// chunks sit is checked in the same pass (`Rules::Pack`), or, given
    /// `sorted` (`Rules::P5z`), after it (`open_p5z`, `check_placement`).
    fn validate(&self, mut sorted: Option<&mut [u64]>) -> Result<()> {
        if sorted
            .as_ref()
            .is_some_and(|s| s.len() != self.chunk_count as usize)
        {
            return Err(Error::Invalid);
        }
        let (mut next_chunk, mut next_string, mut logical_total) = (0u32, 0u64, 0u64);
        let mut bufs = [[0u8; PATH_BUF]; 2];
        let mut previous: Option<(u64, usize)> = None;
        let (mut current_volume, mut next_offset) = (0u32, VOLUME_HEADER_SIZE as u64);
        let mut current_size = if self.volume_count > 0 {
            self.volume_size(0)?
        } else {
            0
        };
        for id in 0..self.file_count {
            let raw = self.raw_file(id)?;
            let file = self.parse_file(&raw)?;
            let [a, b] = &mut bufs;
            let (cur, prev) = if id & 1 == 0 { (a, &*b) } else { (b, &*a) };
            if usize::from(file.path_len) >= PATH_BUF {
                return Err(Error::Invalid);
            }
            let path = self.path(&file, cur)?;
            if u64::from(u32_at(&raw, 20)?) != next_string
                || u32_at(&raw, 28)? != 0
                || u16_at(&raw, 26)? & !1 != 0
                || !valid_path(path)
                || file.path_hash != path_hash(path)
                || file.first_chunk != next_chunk
                || previous.is_some_and(|(h, n)| (h, &prev[..n]) >= (file.path_hash, path))
            {
                return Err(Error::Invalid);
            }
            next_string += u64::from(file.path_len);
            next_chunk = next_chunk
                .checked_add(file.chunk_count)
                .ok_or(Error::Invalid)?;
            logical_total = logical_total
                .checked_add(file.logical_size)
                .ok_or(Error::Invalid)?;
            previous = Some((file.path_hash, path.len()));
            for n in 0..file.chunk_count {
                let chunk_id = file.first_chunk + n;
                let chunk = self.chunk(chunk_id)?;
                let decoded = (file.logical_size - u64::from(n) * CHUNK_SIZE).min(CHUNK_SIZE);
                if chunk.codec > 1
                    || u32::from(chunk.volume_id) >= self.volume_count
                    || (chunk.codec == 0 && u64::from(chunk.stored_size) != decoded)
                    || (chunk.codec == 1 && u64::from(chunk.stored_size) >= decoded)
                    || (file.store_only && chunk.codec != 0)
                {
                    return Err(Error::Invalid);
                }
                // `chunk` read `chunk_id`, so it is below the chunk count,
                // which is `sorted`'s length.
                if let Some(sorted) = sorted.as_deref_mut() {
                    *sorted.get_mut(chunk_id as usize).ok_or(Error::Invalid)? = tile_key(&chunk);
                    continue;
                }
                let volume = u32::from(chunk.volume_id);
                if volume != current_volume {
                    if volume != current_volume + 1 || next_offset != current_size {
                        return Err(Error::Invalid);
                    }
                    current_volume = volume;
                    current_size = self.volume_size(volume)?;
                    next_offset = VOLUME_HEADER_SIZE as u64;
                }
                if u64::from(chunk.offset) != next_offset {
                    return Err(Error::Invalid);
                }
                next_offset = next_offset
                    .checked_add(u64::from(chunk.stored_size))
                    .ok_or(Error::Invalid)?;
                if next_offset > current_size {
                    return Err(Error::Invalid);
                }
            }
        }
        if next_string != self.strings_len
            || next_chunk != self.chunk_count
            || logical_total != self.total_logical_bytes
        {
            return Err(Error::Invalid);
        }
        for volume in 0..self.volume_count {
            if !(VOLUME_HEADER_SIZE as u64..=VOLUME_LIMIT).contains(&self.volume_size(volume)?) {
                return Err(Error::Invalid);
            }
        }
        // `Rules::P5z`: `open_p5z` checks where the chunks sit, from
        // `sorted`.
        if sorted.is_some() {
            return Ok(());
        }
        if self.volume_count > 0
            && (current_volume + 1 != self.volume_count
                || next_offset != self.volume_size(current_volume)?)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    /// `Rules::P5z`, after the record pass filled `sorted` with every
    /// chunk's `tile_key`: sorted by volume and offset, no chunk starts
    /// before the end of the one before it, and the last one ends inside
    /// its volume. Room between chunks, or a volume with no chunk, is
    /// allowed: it is what files an update changed or removed leave. The
    /// record pass already refused a chunk in a volume past the count or
    /// inside the volume header. Sorts in place, with no recursion and no
    /// memory but `sorted` (`sort_u64`).
    fn check_placement(&self, sorted: &mut [u64]) -> core::result::Result<(), P5zOpenError> {
        crate::sort::sort_u64(sorted);
        let mut chunks = sorted.iter().map(|&k| untile_key(k)).peekable();
        for volume in 0..self.volume_count {
            let mut end = VOLUME_HEADER_SIZE as u64;
            while let Some((_, offset, stored)) = chunks.next_if(|c| c.0 == volume) {
                if offset < end {
                    return Err(P5zOpenError::Overlap { volume });
                }
                // A u32 offset plus at most 256 KiB: no overflow.
                end = offset + stored;
            }
            if end > self.volume_size(volume)? {
                return Err(P5zOpenError::PastEnd { volume });
            }
        }
        match chunks.next() {
            Some(_) => Err(P5zOpenError::Index(Error::Invalid)),
            None => Ok(()),
        }
    }
}

/// Where a chunk sits, as one u64 that sorts by volume, then offset:
/// volume id (12 bits) at bit 50, offset (32 bits) at bit 18, stored size
/// less one (18 bits) at bit 0. The record's fields all fit, as unpacked.
fn tile_key(c: &ChunkRecord) -> u64 {
    (u64::from(c.volume_id) << 50) | (u64::from(c.offset) << 18) | u64::from(c.stored_size - 1)
}

/// `tile_key` back to (volume, offset, stored size).
fn untile_key(k: u64) -> (u32, u64, u64) {
    (
        (k >> 50) as u32,
        (k >> 18) & u64::from(u32::MAX),
        (k & ((1 << 18) - 1)) + 1,
    )
}

fn valid_path(path: &[u8]) -> bool {
    core::str::from_utf8(path).is_ok_and(format::valid_path)
}

/// (files, chunks, volumes) declared by an index header, before `open`
/// checks them: lets the caller size its tables before the full pass.
pub fn header_counts(h: &[u8]) -> Result<(u64, u64, u32)> {
    if h.get(..8) != Some(&INDEX_MAGIC[..]) {
        return Err(Error::Invalid);
    }
    Ok((u64_at(h, 40)?, u64_at(h, 48)?, u32_at(h, 20)?))
}

/// What the p5z start-up check reads from an index header before the mount
/// (ampr-sys `rt/pack/p5z.rs`): the volume count, the archive id and where
/// the table of volume sizes starts (8 bytes per volume).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderVolumes {
    pub count: u32,
    pub archive_id: [u8; 16],
    pub volumes_at: u64,
}

/// Reads `HeaderVolumes` from the first 128 bytes of an index, with every
/// check of `Index::open` that needs only those bytes (`Header`), so the
/// start-up check refuses every header the mount refuses. The payload CRC,
/// the end of the file and the records are `Index::open`'s, at the mount.
pub fn header_volumes(h: &[u8]) -> Result<HeaderVolumes> {
    let h: &[u8; INDEX_HEADER_SIZE] = h
        .get(..INDEX_HEADER_SIZE)
        .and_then(|h| h.try_into().ok())
        .ok_or(Error::Truncated)?;
    let header = Header::read(h)?;
    header.check(None)?;
    Ok(HeaderVolumes {
        count: header.volume_count,
        archive_id: header.archive_id,
        volumes_at: header.volumes_off,
    })
}

/// Parses a 64-byte header, or the first 64 bytes of a complete volume, and
/// matches it to the checked index and its declared size for volume `id`,
/// under the rules the index was opened with.
pub fn volume_header<S: Source + ?Sized>(
    bytes: &[u8],
    index: &Index<'_, S>,
    id: u32,
) -> Result<VolumeHeader> {
    volume_header_for(
        bytes,
        id,
        index.volume_count(),
        index.archive_id(),
        index.volume_size(id)?,
        index.rules(),
    )
}

/// `volume_header` against the index's values instead of a mounted index:
/// volume `id` of `count`, of archive `archive_id`, `volume_size` bytes
/// with its header. Under `Rules::Pack` the header counts `count` volumes;
/// under `Rules::P5z` at most `count`, since a header written before the
/// `.p5z` grew counts the volumes it had then. The p5z start-up check runs
/// it before the mount.
pub fn volume_header_for(
    bytes: &[u8],
    id: u32,
    count: u32,
    archive_id: &[u8; 16],
    volume_size: u64,
    rules: Rules,
) -> Result<VolumeHeader> {
    let header = format::decode_volume_header(bytes).map_err(|_| Error::Invalid)?;
    let count_ok = match rules {
        Rules::Pack => header.count == count,
        Rules::P5z => header.count <= count,
    };
    if header.id != id
        || !count_ok
        || header.archive_id != *archive_id
        || header.payload_bytes.checked_add(VOLUME_HEADER_SIZE as u64) != Some(volume_size)
    {
        return Err(Error::Invalid);
    }
    Ok(header)
}

/// Writes `ampr_zstd-000.pak` for volume zero, with additional digits past 999.
pub fn volume_name(id: u32, out: &mut [u8]) -> Result<&str> {
    if id as usize >= MAX_VOLUMES {
        return Err(Error::OutOfRange);
    }
    format::write_volume_name(id, out).map_err(|_| Error::Truncated)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::crc32;
    use core::cell::Cell;
    use std::vec::Vec;

    fn record(offset: u32, stored_size: u32, codec: u64) -> u64 {
        u64::from(offset) | (u64::from(stored_size - 1) << 44) | (codec << 62)
    }

    /// One file of a full raw chunk plus a 10-byte tail stored as 9 zstd bytes.
    fn image(shift: u8, tail_codec: u64) -> Vec<u8> {
        image_with(b"a.bin", shift, tail_codec)
    }

    fn image_with(path: &[u8], shift: u8, tail_codec: u64) -> Vec<u8> {
        let size = CHUNK_SIZE + 10;
        let first = VOLUME_HEADER_SIZE as u32;
        let records = [
            record(first, CHUNK_SIZE as u32, 0),
            record(first + CHUNK_SIZE as u32, 9, tail_codec),
        ];
        let mut out = std::vec![0u8; INDEX_HEADER_SIZE];
        out.extend_from_slice(&path_hash(path).to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // first_chunk
        out.extend_from_slice(&0u32.to_le_bytes()); // path_off
        out.extend_from_slice(&(path.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0; 6]); // flags, reserved
        records
            .iter()
            .for_each(|r| out.extend_from_slice(&r.to_le_bytes()));
        let volume = VOLUME_HEADER_SIZE as u64 + CHUNK_SIZE + 9;
        out.extend_from_slice(&volume.to_le_bytes());
        out.extend_from_slice(path);
        let h = &mut out[..INDEX_HEADER_SIZE];
        h[..8].copy_from_slice(INDEX_MAGIC);
        h[8..12].copy_from_slice(&1u32.to_le_bytes());
        h[12..16].copy_from_slice(&(INDEX_HEADER_SIZE as u32).to_le_bytes());
        h[16] = shift;
        h[17] = 5;
        h[20..24].copy_from_slice(&1u32.to_le_bytes());
        h[24..40].copy_from_slice(&[7; 16]);
        for (at, value) in [
            (40, 1u64),
            (48, 2),
            (56, 128),
            (64, 160),
            (72, 176),
            (80, 184),
            (88, path.len() as u64),
            (96, size),
        ] {
            h[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        // The header CRC covers the header with bytes 112..120 still zero.
        let header_crc = crc32(&out[..INDEX_HEADER_SIZE]);
        let payload_crc = crc32(&out[INDEX_HEADER_SIZE..]);
        out[112..116].copy_from_slice(&payload_crc.to_le_bytes());
        out[116..120].copy_from_slice(&header_crc.to_le_bytes());
        out
    }

    #[test]
    fn index_reads_256_kib_chunks_and_rejects_others() {
        let bytes = image(CHUNK_SHIFT, 1);
        let index = Index::open(&bytes[..]).unwrap();
        let file = index.file(0).unwrap();
        assert_eq!(file.chunk_count, 2);
        let full = index.chunk(0).unwrap();
        assert_eq!((full.offset, full.volume_id), (64, 0));
        assert_eq!((full.stored_size, full.codec), (1 << 18, 0));
        let tail = index.chunk(1).unwrap();
        assert_eq!(
            (tail.offset, tail.stored_size, tail.codec),
            (64 + (1 << 18), 9, 1)
        );
        // A 64 KiB (shift 16) index fails cleanly.
        assert_eq!(Index::open(&image(16, 1)[..]).err(), Some(Error::Invalid));
        for codec in [2, 3] {
            assert_eq!(
                Index::open(&image(CHUNK_SHIFT, codec)[..]).err(),
                Some(Error::Invalid)
            );
        }
        let mut buf = [0u8; 32];
        assert_eq!(volume_name(7, &mut buf).unwrap(), "ampr_zstd-007.pak");
        assert_eq!(
            volume_name(MAX_VOLUMES as u32, &mut buf).err(),
            Some(Error::OutOfRange)
        );
    }

    /// A source that is not a slice: counts reads and can fail like a file.
    struct Counting {
        bytes: Vec<u8>,
        reads: Cell<usize>,
        fail: Cell<bool>,
    }

    impl Source for Counting {
        fn len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&self, at: u64, out: &mut [u8]) -> Result<()> {
            self.reads.set(self.reads.get() + 1);
            if self.fail.get() {
                return Err(Error::Io);
            }
            self.bytes[..].read_at(at, out)
        }
    }

    #[test]
    fn index_reads_through_a_non_slice_source() {
        let bytes = image(CHUNK_SHIFT, 1);
        let src = Counting {
            bytes: bytes.clone(),
            reads: Cell::new(0),
            fail: Cell::new(false),
        };
        let (paged, plain) = (Index::open(&src).unwrap(), Index::open(&bytes[..]).unwrap());
        assert!(src.reads.get() > 0);
        assert_eq!(paged.find(b"a.bin"), plain.find(b"a.bin"));
        assert_eq!(paged.find(b"a.bin"), Ok(Some(0)));
        assert_eq!(paged.find(b"b.bin"), Ok(None));
        let (a, b) = (paged.file(0).unwrap(), plain.file(0).unwrap());
        assert_eq!(a, b);
        let (mut x, mut y) = ([0u8; PATH_BUF], [0u8; PATH_BUF]);
        assert_eq!(paged.path(&a, &mut x), plain.path(&b, &mut y));
        assert_eq!(paged.path(&a, &mut x).unwrap(), b"a.bin");
        assert_eq!(paged.path(&a, &mut [0u8; 4]).err(), Some(Error::Invalid));
        assert_eq!(paged.chunk(1), plain.chunk(1));
        assert_eq!(paged.volume_size(0), plain.volume_size(0));
        // A path of PATH_BUF bytes or longer is refused (the PS5 path limit).
        let long = [b'a'; PATH_BUF];
        let ok = image_with(&long[..PATH_BUF - 1], CHUNK_SHIFT, 1);
        assert!(Index::open(&ok[..]).is_ok());
        let too_long = image_with(&long, CHUNK_SHIFT, 1);
        assert_eq!(Index::open(&too_long[..]).err(), Some(Error::Invalid));
        src.fail.set(true);
        assert_eq!(paged.file(0).err(), Some(Error::Io));
        assert_eq!(Index::open(&src).err(), Some(Error::Io));
    }

    /// `n` small raw files of one chunk each in one volume, records in
    /// (hash, path) order.
    fn multi(n: usize) -> Vec<u8> {
        let mut files: Vec<(u64, Vec<u8>)> = (0..n)
            .map(|i| std::format!("d{}/f{i}.bin", i % 5).into_bytes())
            .map(|p| (path_hash(&p), p))
            .collect();
        files.sort();
        let size = |i: usize| 10 + (i % 7) as u64;
        let (mut recs, mut chunks, mut strings) = (Vec::new(), Vec::new(), Vec::new());
        let mut offset = VOLUME_HEADER_SIZE as u64;
        for (i, (hash, path)) in files.iter().enumerate() {
            recs.extend_from_slice(&hash.to_le_bytes());
            recs.extend_from_slice(&size(i).to_le_bytes());
            recs.extend_from_slice(&(i as u32).to_le_bytes());
            recs.extend_from_slice(&(strings.len() as u32).to_le_bytes());
            recs.extend_from_slice(&(path.len() as u16).to_le_bytes());
            recs.extend_from_slice(&[0; 6]);
            chunks.extend_from_slice(&record(offset as u32, size(i) as u32, 0).to_le_bytes());
            offset += size(i);
            strings.extend_from_slice(path);
        }
        let mut out = std::vec![0u8; INDEX_HEADER_SIZE];
        let chunks_at = (INDEX_HEADER_SIZE + recs.len()) as u64;
        let volumes_at = chunks_at + chunks.len() as u64;
        out.extend_from_slice(&recs);
        out.extend_from_slice(&chunks);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&strings);
        let h = &mut out[..INDEX_HEADER_SIZE];
        h[..8].copy_from_slice(INDEX_MAGIC);
        h[8..12].copy_from_slice(&1u32.to_le_bytes());
        h[12..16].copy_from_slice(&(INDEX_HEADER_SIZE as u32).to_le_bytes());
        h[16] = CHUNK_SHIFT;
        h[17] = 5;
        h[20..24].copy_from_slice(&1u32.to_le_bytes());
        let total = offset - VOLUME_HEADER_SIZE as u64;
        for (at, value) in [
            (40, n as u64),
            (48, n as u64),
            (56, INDEX_HEADER_SIZE as u64),
            (64, chunks_at),
            (72, volumes_at),
            (80, volumes_at + 8),
            (88, strings.len() as u64),
            (96, total),
        ] {
            h[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        let header_crc = crc32(&out[..INDEX_HEADER_SIZE]);
        let payload_crc = crc32(&out[INDEX_HEADER_SIZE..]);
        out[112..116].copy_from_slice(&payload_crc.to_le_bytes());
        out[116..120].copy_from_slice(&header_crc.to_le_bytes());
        out
    }

    #[test]
    fn chunk_tables_answer_from_memory_and_leave_files_in_the_source() {
        // 600 chunks: the pass crosses 4 KiB blocks of the chunk table.
        let n = 600;
        let bytes = multi(n);
        let src = Counting {
            bytes: bytes.clone(),
            reads: Cell::new(0),
            fail: Cell::new(false),
        };
        let plain = Index::open(&bytes[..]).unwrap();
        let index = Index::open(&src).unwrap();
        assert_eq!(index.chunk_lens(), (n, 1));
        let (c, v) = index.chunk_lens();
        let (mut chunks, mut volumes) = (std::vec![0; c], std::vec![0; v]);
        assert_eq!(
            index.fill_chunks(&mut chunks[1..], &mut volumes),
            Err(Error::Invalid)
        );
        assert_eq!(index.fill_chunks(&mut chunks, &mut []), Err(Error::Invalid));
        index.fill_chunks(&mut chunks, &mut volumes).unwrap();
        for (c, v) in [(&chunks[1..], &volumes[..]), (&chunks[..], &[][..])] {
            assert_eq!(
                Index::open(&src).unwrap().with_chunks(c, v).err(),
                Some(Error::Invalid)
            );
        }
        let index = index.with_chunks(&chunks, &volumes).unwrap();
        assert!(index.has_chunks() && !plain.has_chunks());
        // Chunks and volumes from memory: a failing source is never read.
        src.fail.set(true);
        src.reads.set(0);
        for id in 0..=n as u32 {
            assert_eq!(index.chunk(id), plain.chunk(id));
        }
        assert_eq!(index.volume_size(0), plain.volume_size(0));
        assert_eq!(index.volume_size(1), plain.volume_size(1));
        assert_eq!(src.reads.get(), 0);
        // Files, sizes, paths and lookups read the source as without a table.
        assert_eq!(index.file(0).err(), Some(Error::Io));
        assert_eq!(index.logical_size(0).err(), Some(Error::Io));
        assert_eq!(index.find(b"none.bin").err(), Some(Error::Io));
        assert!(src.reads.get() > 0);
        src.fail.set(false);
        let mut buf = [0u8; PATH_BUF];
        for id in 0..n as u64 {
            let rec = index.file(id).unwrap();
            assert_eq!(rec, plain.file(id).unwrap());
            assert_eq!(index.logical_size(id), Ok(rec.logical_size));
            let path = index.path(&rec, &mut buf).unwrap().to_vec();
            assert_eq!(index.find(&path), Ok(Some(id)));
        }
        assert_eq!(index.logical_size(n as u64), Err(Error::OutOfRange));
    }

    /// Recomputes the header CRC (116) of an edited index header, keeping
    /// its payload CRC (112).
    fn reseal(h: &mut [u8]) {
        let payload = u32_at(h, 112).unwrap();
        h[112..120].fill(0);
        let crc = crc32(&h[..INDEX_HEADER_SIZE]);
        h[112..116].copy_from_slice(&payload.to_le_bytes());
        h[116..120].copy_from_slice(&crc.to_le_bytes());
    }

    #[test]
    fn header_volumes_finds_the_volume_table() {
        let bytes = multi(3);
        let index = Index::open(&bytes[..]).unwrap();
        let v = header_volumes(&bytes).unwrap();
        assert_eq!(v.count, index.volume_count());
        assert_eq!(&v.archive_id, index.archive_id());
        let at = v.volumes_at as usize;
        assert_eq!(
            u64_at(&bytes, at),
            Ok(index.volume_size(0).unwrap()),
            "the table holds the volume sizes"
        );
        // A resealed copy of the header passes; only the header is read.
        let mut h = bytes[..INDEX_HEADER_SIZE].to_vec();
        reseal(&mut h);
        assert_eq!(header_volumes(&h), Ok(v));

        assert_eq!(
            header_volumes(&bytes[..INDEX_HEADER_SIZE - 1]),
            Err(Error::Truncated)
        );
        for (at, value) in [(0usize, b'X'), (8, 2), (12, 64)] {
            let mut bad = h.clone();
            bad[at] = value;
            reseal(&mut bad);
            assert_eq!(header_volumes(&bad), Err(Error::Invalid), "byte {at}");
        }
        let mut bad = h.clone();
        bad[20..24].copy_from_slice(&(MAX_VOLUMES as u32 + 1).to_le_bytes());
        reseal(&mut bad);
        assert_eq!(header_volumes(&bad), Err(Error::Invalid));
        // An edit without a new CRC: the archive id, the count, the table.
        for at in [30usize, 20, 72] {
            let mut bad = h.clone();
            bad[at] ^= 1;
            assert_eq!(header_volumes(&bad), Err(Error::Checksum), "byte {at}");
        }
    }

    /// Every header the mount refuses on its 128 bytes alone is refused at
    /// start-up too, with the same error: each edit is resealed, so only
    /// its own rule fails, and the payload CRC still matches.
    #[test]
    fn header_volumes_refuses_what_index_open_refuses_in_the_header() {
        type Edit = fn(&mut [u8]);
        let edits: [(&str, Edit); 10] = [
            ("chunk shift 16", |h| h[16] = 16),
            ("zstd level 0", |h| h[17] = 0),
            ("zstd level 23", |h| h[17] = 23),
            ("reserved u16 at 18", |h| h[18] = 1),
            ("reserved u64 at 104", |h| h[104] = 1),
            ("reserved u64 at 120", |h| h[120] = 1),
            ("chunk count over u32::MAX", |h| {
                h[48..56].copy_from_slice(&(u64::from(u32::MAX) + 1).to_le_bytes())
            }),
            ("files not right after the header", |h| {
                h[56..64].copy_from_slice(&129u64.to_le_bytes())
            }),
            ("chunk table not before the volume table", |h| {
                h[72..80].copy_from_slice(&1u64.to_le_bytes())
            }),
            ("chunks with no volume", |h| h[20..24].fill(0)),
        ];
        let mut passed = Vec::new();
        for (what, edit) in edits {
            let mut bytes = multi(3);
            edit(&mut bytes[..INDEX_HEADER_SIZE]);
            reseal(&mut bytes[..INDEX_HEADER_SIZE]);
            let mount = Index::open(&bytes[..]).err();
            assert_eq!(mount, Some(Error::Invalid), "{what}: the mount refuses it");
            for h in [&bytes[..], &bytes[..INDEX_HEADER_SIZE]] {
                if header_volumes(h).err() != mount {
                    passed.push((what, h.len()));
                }
            }
        }
        assert!(passed.is_empty(), "start-up passed: {passed:?}");
    }

    /// An index of raw files of `sizes` bytes, records in (hash, path)
    /// order: record `i` is `sizes[i]` bytes, its chunks right after those
    /// of the records before it. `layout[v]` lists the chunks volume `v`
    /// holds, by chunk id, in the order they sit there: back to back from
    /// byte 64, the volume ending with its last one. `edit` may change the
    /// chunk records and the volume sizes before the CRCs are sealed.
    fn laid_out(
        sizes: &[u64],
        layout: &[&[u32]],
        edit: impl FnOnce(&mut [u64], &mut [u64]),
    ) -> Vec<u8> {
        let mut paths: Vec<(u64, Vec<u8>)> = (0..sizes.len())
            .map(|i| std::format!("f{i}.bin").into_bytes())
            .map(|p| (path_hash(&p), p))
            .collect();
        paths.sort();
        let raw: Vec<u64> = sizes
            .iter()
            .flat_map(|&s| {
                (0..s.div_ceil(CHUNK_SIZE)).map(move |n| (s - n * CHUNK_SIZE).min(CHUNK_SIZE))
            })
            .collect();
        let mut chunks = std::vec![0u64; raw.len()];
        let mut volumes = Vec::new();
        for (v, ids) in layout.iter().enumerate() {
            let mut offset = VOLUME_HEADER_SIZE as u64;
            for &id in *ids {
                let stored = raw[id as usize];
                chunks[id as usize] = offset | ((v as u64) << 32) | ((stored - 1) << 44);
                offset += stored;
            }
            volumes.push(offset);
        }
        edit(&mut chunks, &mut volumes);
        let (mut recs, mut strings, mut first) = (Vec::new(), Vec::new(), 0u32);
        for ((hash, path), &size) in paths.iter().zip(sizes) {
            recs.extend_from_slice(&hash.to_le_bytes());
            recs.extend_from_slice(&size.to_le_bytes());
            recs.extend_from_slice(&first.to_le_bytes());
            recs.extend_from_slice(&(strings.len() as u32).to_le_bytes());
            recs.extend_from_slice(&(path.len() as u16).to_le_bytes());
            recs.extend_from_slice(&[0; 6]);
            strings.extend_from_slice(path);
            first += size.div_ceil(CHUNK_SIZE) as u32;
        }
        let chunks_at = (INDEX_HEADER_SIZE + recs.len()) as u64;
        let volumes_at = chunks_at + 8 * chunks.len() as u64;
        let strings_at = volumes_at + 8 * volumes.len() as u64;
        let mut out = std::vec![0u8; INDEX_HEADER_SIZE];
        out.extend_from_slice(&recs);
        chunks
            .iter()
            .chain(&volumes)
            .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        out.extend_from_slice(&strings);
        let h = &mut out[..INDEX_HEADER_SIZE];
        h[..8].copy_from_slice(INDEX_MAGIC);
        h[8..12].copy_from_slice(&1u32.to_le_bytes());
        h[12..16].copy_from_slice(&(INDEX_HEADER_SIZE as u32).to_le_bytes());
        h[16] = CHUNK_SHIFT;
        h[17] = 5;
        h[20..24].copy_from_slice(&(volumes.len() as u32).to_le_bytes());
        h[24..40].copy_from_slice(&[9; 16]);
        for (at, value) in [
            (40, sizes.len() as u64),
            (48, chunks.len() as u64),
            (56, INDEX_HEADER_SIZE as u64),
            (64, chunks_at),
            (72, volumes_at),
            (80, strings_at),
            (88, strings.len() as u64),
            (96, sizes.iter().sum()),
        ] {
            h[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        let header_crc = crc32(&out[..INDEX_HEADER_SIZE]);
        let payload_crc = crc32(&out[INDEX_HEADER_SIZE..]);
        out[112..116].copy_from_slice(&payload_crc.to_le_bytes());
        out[116..120].copy_from_slice(&header_crc.to_le_bytes());
        out
    }

    /// How `bytes` opens with `Index::open` and with `Index::open_p5z`.
    fn open_both(bytes: &[u8]) -> (Result<()>, core::result::Result<(), P5zOpenError>) {
        let (_, chunks, _) = header_counts(bytes).unwrap();
        let mut sorted = std::vec![0u64; chunks as usize];
        (
            Index::open(bytes).map(|_| ()),
            Index::open_p5z(bytes, &mut sorted).map(|_| ()),
        )
    }

    /// One chunk per file; record 5 is a whole 256 KiB chunk, the largest
    /// stored size.
    const SIZES: [u64; 6] = [100, 2000, 30, 4096, 777, CHUNK_SIZE];
    const IN_ORDER: [&[u32]; 2] = [&[0, 1, 2], &[3, 4, 5]];
    /// Each volume full, its chunks in another order than the records'.
    const OUT_OF_ORDER: [&[u32]; 2] = [&[2, 0, 1], &[5, 3, 4]];
    const PACK_REFUSES: Result<()> = Err(Error::Invalid);
    const P5Z_TAKES: core::result::Result<(), P5zOpenError> = Ok(());
    /// Refused by a check every index makes, before where the chunks sit.
    const BOTH_INVALID: (Result<()>, core::result::Result<(), P5zOpenError>) =
        (PACK_REFUSES, Err(P5zOpenError::Index(Error::Invalid)));

    fn overlap(volume: u32) -> (Result<()>, core::result::Result<(), P5zOpenError>) {
        (PACK_REFUSES, Err(P5zOpenError::Overlap { volume }))
    }

    fn past_end(volume: u32) -> (Result<()>, core::result::Result<(), P5zOpenError>) {
        (PACK_REFUSES, Err(P5zOpenError::PastEnd { volume }))
    }

    #[test]
    fn chunks_in_record_order_open_with_both_rules() {
        let bytes = laid_out(&SIZES, &IN_ORDER, |_, _| {});
        assert_eq!(open_both(&bytes), (Ok(()), Ok(())));
        let pack = Index::open(&bytes[..]).unwrap();
        let mut sorted = [0u64; SIZES.len()];
        let p5z = Index::open_p5z(&bytes[..], &mut sorted).unwrap();
        assert_eq!((pack.rules(), p5z.rules()), (Rules::Pack, Rules::P5z));
        for id in 0..=SIZES.len() as u32 {
            assert_eq!(p5z.chunk(id), pack.chunk(id));
        }
        // An index with no chunk and no volume (one empty file).
        assert_eq!(open_both(&laid_out(&[0], &[], |_, _| {})), (Ok(()), Ok(())));
    }

    /// What appending to a `.p5z` leaves: the records are re-sorted by
    /// path hash, so their chunks no longer follow record order, and a
    /// record's chunk can sit in an earlier volume than the one before it.
    #[test]
    fn chunks_out_of_record_order_open_with_p5z_rules_only() {
        let layouts: [[&[u32]; 2]; 4] = [
            OUT_OF_ORDER,
            [&[3, 4, 5], &[0, 1, 2]],
            [&[5, 0], &[1, 3, 2, 4]],
            [&[4, 2, 0], &[1, 5, 3]],
        ];
        for layout in layouts {
            let bytes = laid_out(&SIZES, &layout, |_, _| {});
            assert_eq!(open_both(&bytes), (PACK_REFUSES, P5Z_TAKES), "{layout:?}");
        }
    }

    /// Offsets up to the volume limit: a file of 16,383 whole chunks in
    /// one volume of 4,294,705,216 bytes, its chunks laid out backwards.
    #[test]
    fn p5z_rules_sort_offsets_up_to_the_volume_limit() {
        let n = 16_383u32;
        let backwards: Vec<u32> = (0..n).rev().collect();
        let layout: [&[u32]; 2] = [&backwards, &[n]];
        let sizes = [u64::from(n) * CHUNK_SIZE, 10];
        let bytes = laid_out(&sizes, &layout, |_, _| {});
        assert_eq!(open_both(&bytes), (PACK_REFUSES, P5Z_TAKES));
        let mut sorted = std::vec![0u64; n as usize + 1];
        let index = Index::open_p5z(&bytes[..], &mut sorted).unwrap();
        assert_eq!(index.volume_size(0), Ok(64 + u64::from(n) * CHUNK_SIZE));
        assert!(index.volume_size(0).unwrap() <= VOLUME_LIMIT);
        assert_eq!(index.chunk(0).unwrap().offset, 64 + (n - 1) * (1 << 18));
        // One byte short: the first chunk runs past the volume's end.
        let short = laid_out(&sizes, &layout, |_, v| v[0] -= 1);
        assert_eq!(open_both(&short), past_end(0));
    }

    /// The room a file left when an update changed or removed it: a hole
    /// between chunks, at the start or the end of a volume, or a whole
    /// volume with no chunk. p5z rules take it; pack rules do not.
    #[test]
    fn a_hole_or_an_empty_volume_opens_with_p5z_rules_only() {
        let holes: [(&str, [&[u32]; 2], fn(&mut [u64], &mut [u64])); 6] = [
            // The last chunk of volume 0 one byte late, the volume one byte
            // longer.
            ("between chunks", IN_ORDER, |c, v| {
                c[2] += 1;
                v[0] += 1;
            }),
            ("between chunks", OUT_OF_ORDER, |c, v| {
                c[1] += 1;
                v[0] += 1;
            }),
            ("after the header", OUT_OF_ORDER, |c, v| {
                for i in [5, 3, 4] {
                    c[i] += 1;
                }
                v[1] += 1;
            }),
            ("at the end", IN_ORDER, |_, v| v[1] += 1),
            ("at the end", OUT_OF_ORDER, |_, v| v[1] += 1),
            // Chunk 0 written again at the end of volume 1, as an update
            // does: its old place in volume 0 is a hole.
            ("where a chunk was", OUT_OF_ORDER, |c, v| {
                c[0] = (c[0] & !(0xfff_ffff_ffff)) | (1 << 32) | v[1];
                v[1] += SIZES[0];
            }),
        ];
        for (what, layout, edit) in holes {
            let bytes = laid_out(&SIZES, &layout, edit);
            assert_eq!(open_both(&bytes), (PACK_REFUSES, P5Z_TAKES), "{what} {layout:?}");
        }
        // A volume that holds no chunk: only its header, or room that
        // every file in it left.
        for size in [64, 64 + 10_000] {
            let bytes = laid_out(&SIZES, &[&[0, 1, 2], &[], &[3, 4, 5]], |_, v| v[1] = size);
            assert_eq!(open_both(&bytes), (PACK_REFUSES, P5Z_TAKES), "{size}");
        }
    }

    #[test]
    fn an_overlap_is_refused_with_both_rules() {
        // The last chunk of volume 0 one byte early, the volume one byte
        // shorter: it overlaps the chunk before it.
        let bytes = laid_out(&SIZES, &IN_ORDER, |c, v| {
            c[2] -= 1;
            v[0] -= 1;
        });
        assert_eq!(open_both(&bytes), overlap(0));
        let bytes = laid_out(&SIZES, &OUT_OF_ORDER, |c, v| {
            c[1] -= 1;
            v[0] -= 1;
        });
        assert_eq!(open_both(&bytes), overlap(0));
        // One byte early inside volume 1: an overlap before it, a hole after.
        let bytes = laid_out(&SIZES, &OUT_OF_ORDER, |c, _| c[3] -= 1);
        assert_eq!(open_both(&bytes), overlap(1));
        // Two chunks at one offset: chunk 0 moved onto chunk 2.
        let bytes = laid_out(&SIZES, &OUT_OF_ORDER, |c, _| {
            c[0] = (c[0] & !0xffff_ffff) | (c[2] & 0xffff_ffff);
        });
        assert_eq!(open_both(&bytes), overlap(0));
        // The first chunk of a volume inside its header: the chunk record
        // itself is refused.
        let bytes = laid_out(&SIZES, &OUT_OF_ORDER, |c, v| {
            for i in [2, 0, 1] {
                c[i] -= 1;
            }
            v[0] -= 1;
        });
        assert_eq!(open_both(&bytes), BOTH_INVALID);
    }

    #[test]
    fn a_chunk_past_its_volume_is_refused_with_both_rules() {
        for layout in [IN_ORDER, OUT_OF_ORDER] {
            let bytes = laid_out(&SIZES, &layout, |_, v| v[1] -= 1);
            assert_eq!(open_both(&bytes), past_end(1), "{layout:?}");
        }
        // A chunk of volume 0 moved past its end, into what would be
        // volume 1's room.
        let bytes = laid_out(&SIZES, &OUT_OF_ORDER, |c, v| {
            c[1] += v[1];
            v[0] -= SIZES[1];
        });
        assert_eq!(open_both(&bytes), past_end(0));
        // Volume 0 ends where its last chunk starts.
        let bytes = laid_out(&SIZES, &OUT_OF_ORDER, |_, v| v[0] -= SIZES[1]);
        assert_eq!(open_both(&bytes), past_end(0));
    }

    #[test]
    fn a_chunk_in_a_volume_the_index_does_not_have_is_refused() {
        for (layout, volume) in [(IN_ORDER, 2u64), (OUT_OF_ORDER, 2), (OUT_OF_ORDER, 4095)] {
            // The last chunk of volume 1 in volume `volume`, of two.
            let bytes = laid_out(&SIZES, &layout, |c, v| {
                let last = layout[1][2] as usize;
                c[last] = (c[last] & !(0xfff << 32)) | (volume << 32);
                v[1] -= SIZES[last];
            });
            assert_eq!(open_both(&bytes), BOTH_INVALID, "{layout:?} {volume}");
        }
    }

    #[test]
    fn p5z_rules_need_one_sorted_slot_per_chunk() {
        let bytes = laid_out(&SIZES, &OUT_OF_ORDER, |_, _| {});
        for n in [0, SIZES.len() - 1, SIZES.len() + 1] {
            let mut sorted = std::vec![0u64; n];
            assert_eq!(
                Index::open_p5z(&bytes[..], &mut sorted).err(),
                Some(P5zOpenError::Index(Error::Invalid)),
                "{n}"
            );
        }
        // Every other check still runs: a payload CRC, a codec, a total.
        let mut bad = bytes.clone();
        bad[200] ^= 1;
        let checksum = (Err(Error::Checksum), Err(P5zOpenError::Index(Error::Checksum)));
        assert_eq!(open_both(&bad), checksum);
        let zstd_tail = laid_out(&SIZES, &OUT_OF_ORDER, |c, _| c[2] |= 1 << 62);
        assert_eq!(open_both(&zstd_tail), BOTH_INVALID);
    }

    /// A 64-byte AMPRZDT1 header with its CRC.
    fn volume_bytes(id: u32, count: u32, archive_id: [u8; 16], payload: u64) -> [u8; 64] {
        let mut h = [0u8; VOLUME_HEADER_SIZE];
        h[..8].copy_from_slice(b"AMPRZDT1");
        h[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        h[12..16].copy_from_slice(&(VOLUME_HEADER_SIZE as u32).to_le_bytes());
        h[16..20].copy_from_slice(&id.to_le_bytes());
        h[20..24].copy_from_slice(&count.to_le_bytes());
        h[24..40].copy_from_slice(&archive_id);
        h[40..48].copy_from_slice(&payload.to_le_bytes());
        let crc = crc32(&h);
        h[60..64].copy_from_slice(&crc.to_le_bytes());
        h
    }

    #[test]
    fn volume_header_for_makes_the_checks_of_volume_header() {
        let bytes = multi(3);
        let index = Index::open(&bytes[..]).unwrap();
        let size = index.volume_size(0).unwrap();
        let id = *index.archive_id();
        let good = volume_bytes(0, 1, id, size - VOLUME_HEADER_SIZE as u64);
        let header = volume_header(&good, &index, 0).unwrap();
        let pack = Rules::Pack;
        assert_eq!(volume_header_for(&good, 0, 1, &id, size, pack), Ok(header));
        let mut other = id;
        other[3] ^= 1;
        for bad in [
            volume_header_for(&good, 1, 1, &id, size, pack),
            volume_header_for(&good, 0, 2, &id, size, pack),
            volume_header_for(&good, 0, 1, &other, size, pack),
            volume_header_for(&good, 0, 1, &id, size + 1, pack),
            volume_header_for(&good[..63], 0, 1, &id, size, pack),
        ] {
            assert_eq!(bad, Err(Error::Invalid));
        }
        let mut flipped = good;
        flipped[50] ^= 1;
        assert_eq!(
            volume_header_for(&flipped, 0, 1, &id, size, pack),
            Err(Error::Invalid)
        );
        assert_eq!(volume_header(&good, &index, 1), Err(Error::OutOfRange));
    }

    /// A volume header written before the `.p5z` grew counts fewer volumes
    /// than the index: p5z rules take it, pack rules do not. Everything
    /// else in it must still match, with either rules.
    #[test]
    fn p5z_rules_take_a_volume_header_that_counts_fewer_volumes() {
        let bytes = laid_out(&SIZES, &IN_ORDER, |_, _| {});
        let pack = Index::open(&bytes[..]).unwrap();
        let mut sorted = [0u64; SIZES.len()];
        let p5z = Index::open_p5z(&bytes[..], &mut sorted).unwrap();
        let id = *pack.archive_id();
        let size = |v| pack.volume_size(v).unwrap();
        let header = |v: u32, count| volume_bytes(v, count, id, size(v) - 64);
        // Volume 0 as written when the archive had one volume.
        let older = header(0, 1);
        assert_eq!(volume_header(&older, &pack, 0), Err(Error::Invalid));
        let at_most = volume_header_for(&older, 0, 2, &id, size(0), Rules::Pack);
        assert_eq!(at_most, Err(Error::Invalid));
        let taken = volume_header(&older, &p5z, 0).unwrap();
        assert_eq!((taken.id, taken.count), (0, 1));
        let at_most = volume_header_for(&older, 0, 2, &id, size(0), Rules::P5z);
        assert_eq!(at_most, Ok(taken));
        // The index's own count passes with both rules.
        for index in [&pack, &p5z] {
            assert!(volume_header(&header(0, 2), index, 0).is_ok());
            assert!(volume_header(&header(1, 2), index, 1).is_ok());
        }
        // More volumes than the index, another archive, another volume's
        // id, another size: refused with both rules.
        let mut other = id;
        other[0] ^= 1;
        let wrong = [
            header(0, 3),
            volume_bytes(0, 1, other, size(0) - 64),
            volume_bytes(1, 2, id, size(0) - 64),
            volume_bytes(0, 1, id, size(0) - 65),
        ];
        for (n, bytes) in wrong.iter().enumerate() {
            for index in [&pack, &p5z] {
                let rules = index.rules();
                assert_eq!(volume_header(bytes, index, 0), Err(Error::Invalid), "{n} {rules:?}");
                let checked = volume_header_for(bytes, 0, 2, &id, size(0), rules);
                assert_eq!(checked, Err(Error::Invalid), "{n} {rules:?}");
            }
        }
    }
}
