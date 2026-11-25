//! The p5z logic against real `.p5z` files made by the MaxPack engine:
//! one volume (`tests/fixtures/p5z/README.md`) and seven volumes
//! (`tests/fixtures/p5z-multi/README.md`).
use ampr_format::p5z::{
    check_header, check_region, file_offset, parse_image, P5zError, Region, MAX_VOLUMES,
};
use ampr_format::zstd::{header_volumes, volume_header, volume_header_for, Index, Rules};

const P5Z: &[u8] = include_bytes!("fixtures/p5z/basic.p5z");
const INDEX: &[u8] = include_bytes!("fixtures/p5z/ampr_zstd.index");
const PAK: &[u8] = include_bytes!("fixtures/p5z/ampr_zstd-000.pak");
const IMAGE: &[u8] = include_bytes!("fixtures/p5z/maxpack.image");

fn region0() -> Region {
    let mut out = [Region::default(); MAX_VOLUMES];
    let (path, n) = parse_image(IMAGE, &mut out).unwrap();
    assert_eq!((path, n), (&b"p5z/basic.p5z"[..], 1));
    out[0]
}

#[test]
fn fixture_p5z_matches_folder() {
    let index = Index::open(INDEX).unwrap();
    let r = region0();
    check_header(&P5Z[..64], index.archive_id()).unwrap();
    check_region(0, r, P5Z.len() as u64, index.volume_size(0).unwrap()).unwrap();

    let start = r.start as usize;
    volume_header(&P5Z[start..start + 64], &index, 0).unwrap();

    assert!(index.chunk_count() > 0);
    for id in 0..index.chunk_count() {
        let c = index.chunk(id).unwrap();
        assert_eq!(c.volume_id, 0);
        let at = file_offset(r, u64::from(c.offset)).unwrap() as usize;
        let n = c.stored_size as usize;
        assert_eq!(
            &P5Z[at..at + n],
            &PAK[c.offset as usize..c.offset as usize + n],
            "chunk {id}"
        );
    }
    assert_eq!(&P5Z[start..start + r.len as usize], PAK);
}

#[test]
fn fixture_other_archive_id_refused() {
    let index = Index::open(INDEX).unwrap();
    let mut copy = P5Z[..64].to_vec();
    copy[30] ^= 0xff;
    copy[60..64].fill(0);
    let crc = ampr_format::crc32(&copy);
    copy[60..64].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        check_header(&copy, index.archive_id()),
        Err(P5zError::ArchiveId)
    );
}

#[test]
fn fixture_truncated_refused() {
    let index = Index::open(INDEX).unwrap();
    let r = region0();
    let size = index.volume_size(0).unwrap();
    let end = r.start + r.len;
    // The footer would overlap the region by one byte, and a file that
    // ends at the region's end has no footer at all.
    assert_eq!(check_region(0, r, end + 31, size), Err(P5zError::Region(0)));
    assert_eq!(check_region(0, r, end - 1, size), Err(P5zError::Region(0)));
    // The real file holds the region, the index and the footer.
    assert!(P5Z.len() as u64 >= end + 32);
    check_region(0, r, end + 32, size).unwrap();
}

#[test]
fn fixture_checks_without_a_mounted_index() {
    // What the PS5 checks at start-up, before ampr_zstd.index is mounted:
    // the index header, its volume table and each volume header.
    let index = Index::open(INDEX).unwrap();
    let v = header_volumes(&INDEX[..128]).unwrap();
    assert_eq!((v.count, &v.archive_id), (1, index.archive_id()));
    let at = v.volumes_at as usize;
    let size = u64::from_le_bytes(INDEX[at..at + 8].try_into().unwrap());
    assert_eq!(size, index.volume_size(0).unwrap());
    let r = region0();
    check_header(&P5Z[..64], &v.archive_id).unwrap();
    check_region(0, r, P5Z.len() as u64, size).unwrap();
    let h = &P5Z[r.start as usize..r.start as usize + 64];
    assert_eq!(
        volume_header_for(h, 0, v.count, &v.archive_id, size, index.rules()),
        volume_header(h, &index, 0)
    );
    assert!(volume_header_for(h, 0, 1, &v.archive_id, size, Rules::P5z).is_ok());
}

// ---- several volumes (tests/fixtures/p5z-multi) ----

const M_P5Z: &[u8] = include_bytes!("fixtures/p5z-multi/multi.p5z");
const M_INDEX: &[u8] = include_bytes!("fixtures/p5z-multi/ampr_zstd.index");
const M_IMAGE: &[u8] = include_bytes!("fixtures/p5z-multi/maxpack.image");
const M_PAKS: [&[u8]; 7] = [
    include_bytes!("fixtures/p5z-multi/ampr_zstd-000.pak"),
    include_bytes!("fixtures/p5z-multi/ampr_zstd-001.pak"),
    include_bytes!("fixtures/p5z-multi/ampr_zstd-002.pak"),
    include_bytes!("fixtures/p5z-multi/ampr_zstd-003.pak"),
    include_bytes!("fixtures/p5z-multi/ampr_zstd-004.pak"),
    include_bytes!("fixtures/p5z-multi/ampr_zstd-005.pak"),
    include_bytes!("fixtures/p5z-multi/ampr_zstd-006.pak"),
];

fn multi_regions() -> ([Region; MAX_VOLUMES], usize) {
    let mut out = [Region::default(); MAX_VOLUMES];
    let (path, n) = parse_image(M_IMAGE, &mut out).unwrap();
    assert_eq!(path, b"p5z/multi.p5z");
    (out, n)
}

#[test]
fn multi_fixture_matches_folder() {
    let index = Index::open(M_INDEX).unwrap();
    let (regions, n) = multi_regions();
    assert_eq!(n, M_PAKS.len());
    assert_eq!(index.volume_count() as usize, n);
    check_header(&M_P5Z[..64], index.archive_id()).unwrap();

    let mut prev_end = 64;
    for (k, (&r, pak)) in (0u32..).zip(regions[..n].iter().zip(M_PAKS)) {
        let size = index.volume_size(k).unwrap();
        assert_eq!(size, pak.len() as u64, "volume {k}");
        check_region(k, r, M_P5Z.len() as u64, size).unwrap();
        // In order, after the previous one: volume k >= 1 starts past
        // volume 0's end.
        assert!(r.start >= prev_end, "volume {k}");
        prev_end = r.start + r.len;
        let start = r.start as usize;
        volume_header(&M_P5Z[start..start + 64], &index, k).unwrap();
        assert_eq!(&M_P5Z[start..start + r.len as usize], pak, "volume {k}");
    }

    // Every chunk is read from its own volume's region, at the chunk's
    // offset inside that volume, and every volume holds some.
    let mut chunks = [0u32; 7];
    for id in 0..index.chunk_count() {
        let c = index.chunk(id).unwrap();
        let v = usize::from(c.volume_id);
        chunks[v] += 1;
        let at = file_offset(regions[v], u64::from(c.offset)).unwrap() as usize;
        let len = c.stored_size as usize;
        assert_eq!(
            &M_P5Z[at..at + len],
            &M_PAKS[v][c.offset as usize..c.offset as usize + len],
            "chunk {id} of volume {v}"
        );
    }
    assert!(chunks.iter().all(|&c| c > 0), "{chunks:?}");
}

#[test]
fn multi_fixture_checks_without_a_mounted_index() {
    // The start-up check of the PS5: volume k's size at volumes_at + 8k,
    // its region and the header at its start.
    let index = Index::open(M_INDEX).unwrap();
    let v = header_volumes(&M_INDEX[..128]).unwrap();
    assert_eq!((v.count, &v.archive_id), (7, index.archive_id()));
    let (regions, n) = multi_regions();
    check_header(&M_P5Z[..64], &v.archive_id).unwrap();
    for k in 0..n as u32 {
        let at = v.volumes_at as usize + 8 * k as usize;
        let size = u64::from_le_bytes(M_INDEX[at..at + 8].try_into().unwrap());
        assert_eq!(size, index.volume_size(k).unwrap(), "volume {k}");
        let r = regions[k as usize];
        check_region(k, r, M_P5Z.len() as u64, size).unwrap();
        let h = &M_P5Z[r.start as usize..r.start as usize + 64];
        assert_eq!(
            volume_header_for(h, k, v.count, &v.archive_id, size, index.rules()),
            volume_header(h, &index, k),
            "volume {k}"
        );
        assert!(volume_header_for(h, k, v.count, &v.archive_id, size, Rules::P5z).is_ok());
        // Another volume's header at this start is refused: its id is k.
        let other = (k + 1) % v.count;
        for rules in [Rules::Pack, Rules::P5z] {
            assert!(volume_header_for(h, other, v.count, &v.archive_id, size, rules).is_err());
        }
    }
}

#[test]
fn multi_fixture_wrong_region_refused() {
    let index = Index::open(M_INDEX).unwrap();
    let (regions, n) = multi_regions();
    let total = M_P5Z.len() as u64;
    for k in 1..n as u32 {
        let r = regions[k as usize];
        let size = index.volume_size(k).unwrap();
        // Volume 0's region, or its size, is not volume k's.
        if index.volume_size(0).unwrap() != size {
            assert_eq!(
                check_region(k, regions[0], total, size),
                Err(P5zError::Region(k))
            );
        }
        let short = Region {
            len: r.len - 1,
            ..r
        };
        assert_eq!(check_region(k, short, total, size), Err(P5zError::Region(k)));
        // Cut so the footer would overlap the region.
        let end = r.start + r.len;
        assert_eq!(check_region(k, r, end + 31, size), Err(P5zError::Region(k)));
        check_region(k, r, end + 32, size).unwrap();
    }
    // The last region ends before the index and the footer of the file.
    let last = regions[n - 1];
    assert!(last.start + last.len + 32 <= total);
}

// ---- p5z rules: a .p5z that grew by appending ----

/// Volume `k`'s 64-byte header in the multi-volume `.p5z`.
fn m_header(k: usize) -> [u8; 64] {
    let (regions, _) = multi_regions();
    let start = regions[k].start as usize;
    M_P5Z[start..start + 64].try_into().unwrap()
}

/// `h` after `edit`, its CRC (bytes 60..64) sealed again.
fn resealed(mut h: [u8; 64], edit: impl FnOnce(&mut [u8; 64])) -> [u8; 64] {
    edit(&mut h);
    h[60..64].fill(0);
    let crc = ampr_format::crc32(&h);
    h[60..64].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Both real indexes, chunks in record order, open with p5z rules too and
/// read the same chunks; every volume header passes them.
#[test]
fn fixtures_open_with_p5z_rules() {
    for bytes in [INDEX, M_INDEX] {
        let pack = Index::open(bytes).unwrap();
        let mut sorted = vec![0u64; pack.chunk_count() as usize];
        let p5z = Index::open_p5z(bytes, &mut sorted).unwrap();
        assert_eq!((pack.rules(), p5z.rules()), (Rules::Pack, Rules::P5z));
        assert_eq!(p5z.volume_count(), pack.volume_count());
        for id in 0..pack.chunk_count() {
            assert_eq!(p5z.chunk(id), pack.chunk(id), "chunk {id}");
        }
    }
    let mut sorted = vec![0u64; Index::open(M_INDEX).unwrap().chunk_count() as usize];
    let index = Index::open_p5z(M_INDEX, &mut sorted).unwrap();
    for k in 0..M_PAKS.len() {
        volume_header(&m_header(k), &index, k as u32).unwrap();
    }
}

/// Each volume header of the multi-volume `.p5z` as it would read had the
/// archive had only volumes 0..=k when it was written: p5z rules take it,
/// pack rules do not. A header that cannot be (its id not below its count,
/// or more volumes than the index), another archive or another payload
/// size is refused by both.
#[test]
fn multi_fixture_takes_volume_headers_written_before_it_grew() {
    let pack = Index::open(M_INDEX).unwrap();
    let mut sorted = vec![0u64; pack.chunk_count() as usize];
    let p5z = Index::open_p5z(M_INDEX, &mut sorted).unwrap();
    let count = pack.volume_count();
    assert_eq!(count, 7);
    for k in 0..count {
        let older = resealed(m_header(k as usize), |h| {
            h[20..24].copy_from_slice(&(k + 1).to_le_bytes())
        });
        let size = pack.volume_size(k).unwrap();
        let id = pack.archive_id();
        let p5z_ok = volume_header_for(&older, k, count, id, size, Rules::P5z);
        assert_eq!(p5z_ok.map(|h| h.count), Ok(k + 1), "volume {k}");
        assert_eq!(volume_header(&older, &p5z, k), p5z_ok, "volume {k}");
        let pack_ok = volume_header_for(&older, k, count, id, size, Rules::Pack).is_ok();
        assert_eq!(pack_ok, k + 1 == count, "volume {k}");
        assert_eq!(volume_header(&older, &pack, k).is_ok(), pack_ok, "volume {k}");

        let wrong = [
            resealed(m_header(k as usize), |h| {
                h[20..24].copy_from_slice(&k.to_le_bytes())
            }),
            resealed(m_header(k as usize), |h| {
                h[20..24].copy_from_slice(&(count + 1).to_le_bytes())
            }),
            resealed(older, |h| h[30] ^= 0xff),
            resealed(older, |h| h[40] ^= 1),
        ];
        for (n, h) in wrong.iter().enumerate() {
            for index in [&pack, &p5z] {
                assert!(volume_header(h, index, k).is_err(), "volume {k}, {n}");
            }
        }
    }
}

/// `index` with the chunks of each volume laid out backwards: the volume's
/// last chunk id at byte 64, then the one before it, back to back, so every
/// volume is still exactly full. The payload CRC is sealed again.
fn backwards(index: &[u8]) -> Vec<u8> {
    let mut out = index.to_vec();
    let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let (count, chunks_at) = (u64_at(&out, 48) as usize, u64_at(&out, 64) as usize);
    let volumes = u32::from_le_bytes(out[20..24].try_into().unwrap()) as usize;
    let mut ends = vec![64u64; volumes];
    for id in (0..count).rev() {
        let at = chunks_at + 8 * id;
        let c = u64_at(&out, at);
        let v = ((c >> 32) & 0xfff) as usize;
        let moved = (c & !0xffff_ffff) | ends[v];
        ends[v] += ((c >> 44) & 0x3ffff) + 1;
        out[at..at + 8].copy_from_slice(&moved.to_le_bytes());
    }
    let crc = ampr_format::crc32(&out[128..]);
    out[112..116].copy_from_slice(&crc.to_le_bytes());
    out
}

/// The real seven-volume index with its chunks moved, each volume still
/// exactly full: p5z rules open it, pack rules do not. One byte of hole,
/// of overlap, or a chunk in a volume past the count, and neither does.
#[test]
fn multi_fixture_with_chunks_out_of_order_opens_with_p5z_rules_only() {
    let moved = backwards(M_INDEX);
    assert_ne!(moved, M_INDEX);
    let original = Index::open(M_INDEX).unwrap();
    let n = original.chunk_count() as usize;
    let mut sorted = vec![0u64; n];
    assert!(Index::open(&moved[..]).is_err());
    let index = Index::open_p5z(&moved[..], &mut sorted).unwrap();
    for id in 0..n as u32 {
        let (a, b) = (index.chunk(id).unwrap(), original.chunk(id).unwrap());
        assert_eq!(
            (a.volume_id, a.stored_size, a.codec),
            (b.volume_id, b.stored_size, b.codec)
        );
    }
    for k in 0..original.volume_count() {
        assert_eq!(index.volume_size(k), original.volume_size(k));
    }

    // The first two chunks of volume 0 as laid out: the first one byte
    // later (a hole before it) or earlier (inside the volume header), the
    // second one byte earlier (an overlap), or the first in volume 7 of 7.
    let at_offset = |offset: u32| {
        (0..n as u32)
            .find(|&id| {
                let c = index.chunk(id).unwrap();
                (c.volume_id, c.offset) == (0, offset)
            })
            .unwrap()
    };
    let first = at_offset(64);
    let second = at_offset(64 + index.chunk(first).unwrap().stored_size);
    let chunks_at = u64::from_le_bytes(moved[64..72].try_into().unwrap()) as usize;
    let edits: [(&str, u32, fn(u64) -> u64); 4] = [
        ("hole", first, |c| c + 1),
        ("into the header", first, |c| c - 1),
        ("overlap", second, |c| c - 1),
        ("volume 7", first, |c| (c & !(0xfff << 32)) | (7 << 32)),
    ];
    for (what, id, edit) in edits {
        let at = chunks_at + 8 * id as usize;
        let mut broken = moved.clone();
        let c = u64::from_le_bytes(broken[at..at + 8].try_into().unwrap());
        broken[at..at + 8].copy_from_slice(&edit(c).to_le_bytes());
        let crc = ampr_format::crc32(&broken[128..]);
        broken[112..116].copy_from_slice(&crc.to_le_bytes());
        assert!(Index::open(&broken[..]).is_err(), "{what}");
        assert!(Index::open_p5z(&broken[..], &mut sorted).is_err(), "{what}");
    }
}
