//! The p5z logic against a real `.p5z` made by the MaxPack engine
//! (see `tests/fixtures/p5z/README.md`).
use ampr_format::p5z::{
    check_header, check_region, file_offset, parse_image, P5zError, Region, MAX_VOLUMES,
};
use ampr_format::zstd::{volume_header, Index};

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
