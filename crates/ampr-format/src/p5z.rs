//! The pure part of the `p5z` mode: the PS5 reads each zstd volume from a
//! byte region inside one `.p5z` file instead of from a `.pak` file.
//!
//! This module parses `maxpack.image` (which tells where the regions are),
//! checks the 64-byte `.p5z` header and checks every region against the
//! volume sizes of `ampr_zstd.index`. It reserves no memory.
use crate::{crc32_parts, u32_at};

pub const IMAGE_NAME: &str = "maxpack.image";
pub const MAX_VOLUMES: usize = 256;
pub const IMAGE_MAX_BYTES: usize = 12 * 1024;

const IMAGE_MAGIC: &[u8] = b"maxpack-image 1";
/// Line 2 is `p5z/<name>`: the payload mounts the folder that holds the
/// `.p5z` at `/app0/p5z/`.
const MOUNT_PREFIX: &[u8] = b"p5z/";
const HEADER_SIZE: usize = 64;
const FOOTER_SIZE: u64 = 32;

/// Where one volume lives inside the `.p5z`: `len` bytes from `start`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Region {
    pub start: u64,
    pub len: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum P5zError {
    /// `maxpack.image` is malformed (the detail names the rule).
    Image(&'static str),
    /// The `.p5z` header is wrong (the detail names the rule).
    Header(&'static str),
    /// The `.p5z` and `ampr_zstd.index` belong to different archives.
    ArchiveId,
    /// The region of this volume does not fit the `.p5z` or the index.
    Region(u32),
    /// The 64 bytes at the start of this volume's region are not its header.
    Volume(u32),
}

impl P5zError {
    /// Short English text for the notice (`ampr-emu-ng: p5z: <reason>; ...`).
    pub fn reason(&self) -> &'static [u8] {
        match self {
            Self::Image(_) => b"bad maxpack.image",
            Self::Header(_) => b"bad .p5z header",
            Self::ArchiveId => b"archive id mismatch",
            Self::Region(_) => b"volume region out of range",
            Self::Volume(_) => b"bad volume header",
        }
    }
}

/// A decimal `u64`: digits only (no sign, no spaces), no overflow.
fn decimal(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(v)
}

/// A control byte of the contract: below 0x20 or 0x7F. `\n` ends lines.
fn control(b: u8) -> bool {
    b < 0x20 || b == 0x7f
}

/// The `.p5z` file name line 2 carries after `p5z/`: valid UTF-8 (the bytes
/// the kernel's readdir gave the payload, which go back to `open` as they
/// are), not empty, not `.` or `..`, no control byte, no `/` and no `\`.
pub fn image_name_ok(name: &[u8]) -> bool {
    !name.is_empty()
        && name != b"."
        && name != b".."
        && !name.iter().any(|&b| control(b) || b == b'/' || b == b'\\')
        && core::str::from_utf8(name).is_ok()
}

/// Reads `maxpack.image`: returns the `.p5z` path (relative to `/app0`,
/// always `p5z/<name>`) and the number of volumes, whose regions are written
/// to `out[..n]`.
///
/// The text is UTF-8 with `\n` line ends and no other control byte; the
/// final `\n` is optional.
pub fn parse_image<'a>(
    text: &'a [u8],
    out: &mut [Region; MAX_VOLUMES],
) -> Result<(&'a [u8], usize), P5zError> {
    if text.len() > IMAGE_MAX_BYTES {
        return Err(P5zError::Image("too long"));
    }
    if core::str::from_utf8(text).is_err() {
        return Err(P5zError::Image("not UTF-8"));
    }
    if text.iter().any(|&b| b != b'\n' && control(b)) {
        return Err(P5zError::Image("control byte (only LF ends lines)"));
    }
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    let mut lines = body.split(|&b| b == b'\n');
    if lines.next() != Some(IMAGE_MAGIC) {
        return Err(P5zError::Image("first line"));
    }
    let path = lines.next().ok_or(P5zError::Image("no path"))?;
    match path.strip_prefix(MOUNT_PREFIX) {
        Some(name) if image_name_ok(name) => {}
        _ => return Err(P5zError::Image("path")),
    }
    let mut n = 0;
    for line in lines {
        if n == MAX_VOLUMES {
            return Err(P5zError::Image("too many volumes"));
        }
        let mut f = line.split(|&b| b == b' ');
        let (Some(id), Some(start), Some(len), None) = (f.next(), f.next(), f.next(), f.next())
        else {
            return Err(P5zError::Image("volume line"));
        };
        let (Some(id), Some(start), Some(len)) = (decimal(id), decimal(start), decimal(len))
        else {
            return Err(P5zError::Image("volume number"));
        };
        if id != n as u64 {
            return Err(P5zError::Image("volume ids not 0..n in order"));
        }
        out[n] = Region { start, len };
        n += 1;
    }
    if n == 0 {
        return Err(P5zError::Image("no volumes"));
    }
    Ok((path, n))
}

/// Checks the first 64 bytes of a `.p5z`: magic `MAXPKP5Z`, version 1,
/// header size 64, CRC-32 (bytes 60..64, taken as zero) and that its
/// `archive_id` (bytes 24..40) is the one of `ampr_zstd.index`.
pub fn check_header(h: &[u8], index_archive_id: &[u8; 16]) -> Result<(), P5zError> {
    let h = h
        .get(..HEADER_SIZE)
        .ok_or(P5zError::Header("short header"))?;
    if &h[..8] != b"MAXPKP5Z" {
        return Err(P5zError::Header("magic"));
    }
    if u32_at(h, 8) != Ok(1) || u32_at(h, 12) != Ok(HEADER_SIZE as u32) {
        return Err(P5zError::Header("version or size"));
    }
    if u32_at(h, 60) != Ok(crc32_parts(&[&h[..60], &[0; 4]])) {
        return Err(P5zError::Header("crc"));
    }
    if h[24..40] != index_archive_id[..] {
        return Err(P5zError::ArchiveId);
    }
    Ok(())
}

/// Checks the region of volume `id`: as long as the index says, past the
/// header, and ending before the 32-byte footer of a `.p5z` of `p5z_size`.
pub fn check_region(
    id: u32,
    r: Region,
    p5z_size: u64,
    volume_size: u64,
) -> Result<(), P5zError> {
    let fits = r.len == volume_size
        && r.start >= HEADER_SIZE as u64
        && match (r.start.checked_add(r.len), p5z_size.checked_sub(FOOTER_SIZE)) {
            (Some(end), Some(limit)) => end <= limit,
            _ => false,
        };
    if fits {
        Ok(())
    } else {
        Err(P5zError::Region(id))
    }
}

/// Byte offset in the `.p5z` of `logical` bytes into the region; `None`
/// past the region's end or when the sum overflows.
pub fn file_offset(r: Region, logical: u64) -> Option<u64> {
    if logical > r.len {
        return None;
    }
    r.start.checked_add(logical)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::String;
    use std::vec::Vec;

    const GOLDEN: &str = "maxpack-image 1\np5z/basic.p5z\n0 1234 5678\n";

    fn parse(text: &[u8]) -> Result<(Vec<u8>, usize, Region), P5zError> {
        let mut out = [Region::default(); MAX_VOLUMES];
        let (path, n) = parse_image(text, &mut out)?;
        Ok((path.to_vec(), n, out[0]))
    }

    #[test]
    fn image_golden_parses() {
        for text in [GOLDEN, GOLDEN.trim_end_matches('\n')] {
            let (path, n, r) = parse(text.as_bytes()).unwrap();
            assert_eq!(path, b"p5z/basic.p5z");
            assert_eq!(n, 1);
            assert_eq!(
                r,
                Region {
                    start: 1234,
                    len: 5678
                }
            );
        }
        let mut out = [Region::default(); MAX_VOLUMES];
        let (_, n) = parse_image(
            b"maxpack-image 1\np5z/a.p5z\n0 64 10\n1 74 20\n2 94 30\n",
            &mut out,
        )
        .unwrap();
        assert_eq!((n, out[2]), (3, Region { start: 94, len: 30 }));
    }

    /// The shared golden case of the contract: the payload's `image_text`
    /// test writes this exact text. A UTF-8 name as MaxPack's PC app names
    /// a `.p5z` ("<Title> [<TITLE_ID>].p5z").
    const GOLDEN_UTF8: &str =
        "maxpack-image 1\np5z/Starfall’s Odyssey™ 2 [PPSA01234].p5z\n0 1234 5678\n";

    #[test]
    fn image_golden_utf8_parses() {
        let (path, n, r) = parse(GOLDEN_UTF8.as_bytes()).unwrap();
        assert_eq!(
            path,
            "p5z/Starfall’s Odyssey™ 2 [PPSA01234].p5z".as_bytes()
        );
        assert_eq!(
            (n, r),
            (
                1,
                Region {
                    start: 1234,
                    len: 5678
                }
            )
        );
        for name in [
            "God of War Ragnarök [PPSA01411].p5z",
            "グランツーリスモ７ [PPSA01316].p5z",
            "Gran Turismo® 7.p5z",
            "a b.p5z",
            "~!@#$%^&()_+=-{}[];',.p5z",
            "x",
            "...",
            "a.p5z ",
            " a.p5z",
        ] {
            let text = std::format!("maxpack-image 1\np5z/{name}\n0 64 1\n");
            let (path, _, _) = parse(text.as_bytes()).unwrap();
            assert_eq!(path, std::format!("p5z/{name}").as_bytes(), "{name}");
        }
    }

    #[test]
    fn image_refuses_bad_names() {
        let cases: [(&str, &[u8]); 27] = [
            ("invalid UTF-8 byte", b"p5z/a\xff.p5z"),
            ("lone continuation byte", b"p5z/a\x80.p5z"),
            ("overlong slash", b"p5z/a\xc0\xaf.p5z"),
            ("cut sequence", b"p5z/a\xe2\x84.p5z"),
            ("cut sequence at the end", b"p5z/a\xe2\x84"),
            ("surrogate", b"p5z/a\xed\xa0\x80.p5z"),
            ("NUL", b"p5z/a\0.p5z"),
            ("0x01", b"p5z/a\x01.p5z"),
            ("0x1f", b"p5z/a\x1f.p5z"),
            ("tab", b"p5z/a\tb.p5z"),
            ("CR alone", b"p5z/a\rb.p5z"),
            ("CR at the end", b"p5z/a.p5z\r"),
            ("DEL", b"p5z/a\x7f.p5z"),
            ("backslash", b"p5z/a\\b.p5z"),
            ("empty name", b"p5z/"),
            ("dot", b"p5z/."),
            ("dotdot", b"p5z/.."),
            ("no p5z/", b"a.p5z"),
            ("other folder", b"other/a.p5z"),
            ("P5Z/ in capitals", b"P5Z/a.p5z"),
            ("p5z without slash", b"p5z"),
            ("nested", b"p5z/sub/a.p5z"),
            ("double slash", b"p5z//a.p5z"),
            ("dot folder first", b"./p5z/a.p5z"),
            ("absolute", b"/app0/p5z/a.p5z"),
            ("trailing slash", b"p5z/a.p5z/"),
            ("dotdot after", b"p5z/../a.p5z"),
        ];
        for (what, name) in cases {
            let mut text = b"maxpack-image 1\n".to_vec();
            text.extend_from_slice(name);
            text.extend_from_slice(b"\n0 64 1\n");
            assert!(
                matches!(parse(&text), Err(P5zError::Image(_))),
                "{what}: {:?}",
                parse(&text)
            );
        }
        // Invalid UTF-8 or a control byte anywhere else is refused too.
        for text in [
            &b"maxpack-image 1\xff\np5z/a.p5z\n0 64 1\n"[..],
            b"maxpack-image 1\np5z/a.p5z\n0 64 1\n\xc3",
            b"maxpack-image 1\np5z/a.p5z\n0\t64 1\n",
            b"maxpack-image 1\np5z/a.p5z\n0 64 1\x7f\n",
        ] {
            assert!(matches!(parse(text), Err(P5zError::Image(_))), "{text:?}");
        }
    }

    fn volumes(n: usize) -> String {
        let mut s = String::from("maxpack-image 1\np5z/a.p5z\n");
        for i in 0..n {
            s.push_str(&std::format!("{i} 64 1\n"));
        }
        s
    }

    #[test]
    fn image_refuses_bad() {
        let mut oversize = String::from(GOLDEN);
        while oversize.len() <= IMAGE_MAX_BYTES {
            oversize.push_str("# padding line\n");
        }
        let cases: [(&str, String); 24] = [
            ("other first line", "maxpack-image 2\np5z/a.p5z\n0 64 1\n".into()),
            ("first line with suffix", "maxpack-image 1 \np5z/a.p5z\n0 64 1\n".into()),
            ("empty text", String::new()),
            ("no path", "maxpack-image 1\n".into()),
            ("empty path", "maxpack-image 1\n\n0 64 1\n".into()),
            ("absolute path", "maxpack-image 1\n/abs/a.p5z\n0 64 1\n".into()),
            ("dotdot path", "maxpack-image 1\na/../b\n0 64 1\n".into()),
            ("backslash path", "maxpack-image 1\np5z\\a.p5z\n0 64 1\n".into()),
            ("id does not start at 0", "maxpack-image 1\np5z/a.p5z\n1 64 1\n".into()),
            ("id gap", "maxpack-image 1\np5z/a.p5z\n0 64 1\n2 70 1\n".into()),
            ("ids unordered", "maxpack-image 1\np5z/a.p5z\n1 70 1\n0 64 1\n".into()),
            ("zero volumes", "maxpack-image 1\np5z/a.p5z\n".into()),
            ("257 volumes", volumes(257)),
            ("plus sign", "maxpack-image 1\np5z/a.p5z\n0 +64 1\n".into()),
            ("letters", "maxpack-image 1\np5z/a.p5z\n0 6x4 1\n".into()),
            ("hex", "maxpack-image 1\np5z/a.p5z\n0 0x40 1\n".into()),
            (
                "above u64",
                "maxpack-image 1\np5z/a.p5z\n0 99999999999999999999 1\n".into(),
            ),
            ("crlf", "maxpack-image 1\r\np5z/a.p5z\r\n0 64 1\r\n".into()),
            ("extra blank line", "maxpack-image 1\np5z/a.p5z\n0 64 1\n\n".into()),
            ("missing length", "maxpack-image 1\np5z/a.p5z\n0 64\n".into()),
            ("extra field", "maxpack-image 1\np5z/a.p5z\n0 64 1 2\n".into()),
            ("double space", "maxpack-image 1\np5z/a.p5z\n0  64 1\n".into()),
            ("extra line of text", "maxpack-image 1\np5z/a.p5z\n0 64 1\nextra\n".into()),
            ("over IMAGE_MAX_BYTES", oversize),
        ];
        for (what, text) in cases {
            assert!(
                matches!(parse(text.as_bytes()), Err(P5zError::Image(_))),
                "{what}: {:?}",
                parse(text.as_bytes())
            );
        }
        // 256 volumes is the limit and passes.
        assert_eq!(parse(volumes(256).as_bytes()).unwrap().1, 256);
    }

    #[test]
    fn offset_above_4gib() {
        let r = Region {
            start: 5 << 30,
            len: 6 << 30,
        };
        assert_eq!(file_offset(r, 3 << 30), Some((5 << 30) + (3 << 30)));
        assert_eq!(file_offset(r, 6 << 30), Some((5 << 30) + (6 << 30)));
        assert_eq!(file_offset(r, (6 << 30) + 1), None);
        let near_max = Region {
            start: u64::MAX - 10,
            len: 100,
        };
        assert_eq!(file_offset(near_max, 5), Some(u64::MAX - 5));
        assert_eq!(file_offset(near_max, 11), None);
    }

    fn header(id: &[u8; 16]) -> [u8; 64] {
        let mut h = [0u8; 64];
        h[..8].copy_from_slice(b"MAXPKP5Z");
        h[8..12].copy_from_slice(&1u32.to_le_bytes());
        h[12..16].copy_from_slice(&64u32.to_le_bytes());
        h[24..40].copy_from_slice(id);
        refresh_crc(&mut h);
        h
    }

    fn refresh_crc(h: &mut [u8; 64]) {
        h[60..64].fill(0);
        let crc = crate::crc32(&h[..]);
        h[60..64].copy_from_slice(&crc.to_le_bytes());
    }

    #[test]
    fn header_and_region_checks() {
        let id = [7u8; 16];
        let good = header(&id);
        assert_eq!(check_header(&good, &id), Ok(()));

        for (at, val) in [(0usize, b'X'), (8, 2), (12, 65)] {
            let mut bad = good;
            bad[at] = val;
            refresh_crc(&mut bad);
            assert!(matches!(check_header(&bad, &id), Err(P5zError::Header(_))));
        }
        let mut bad = good;
        bad[50] ^= 1; // CRC not refreshed
        assert!(matches!(check_header(&bad, &id), Err(P5zError::Header(_))));
        assert!(matches!(
            check_header(&good[..63], &id),
            Err(P5zError::Header(_))
        ));
        assert!(matches!(check_header(&[], &id), Err(P5zError::Header(_))));

        assert_eq!(check_header(&good, &[8u8; 16]), Err(P5zError::ArchiveId));

        // Region of 1000 bytes at 64, in a file of 64 + 1000 + 32.
        let r = Region {
            start: 64,
            len: 1000,
        };
        assert_eq!(check_region(3, r, 1096, 1000), Ok(()));
        assert_eq!(check_region(3, r, 1096, 999), Err(P5zError::Region(3)));
        assert_eq!(
            check_region(3, Region { len: 999, ..r }, 1096, 1000),
            Err(P5zError::Region(3))
        );
        assert_eq!(
            check_region(
                3,
                Region {
                    start: 63,
                    len: 1000
                },
                1096,
                1000
            ),
            Err(P5zError::Region(3))
        );
        // Overlaps the footer by one byte.
        assert_eq!(check_region(3, r, 1095, 1000), Err(P5zError::Region(3)));
        // Smaller than a footer.
        assert_eq!(check_region(3, r, 31, 1000), Err(P5zError::Region(3)));
        let huge = Region {
            start: u64::MAX - 5,
            len: 10,
        };
        assert_eq!(
            check_region(3, huge, u64::MAX, 10),
            Err(P5zError::Region(3))
        );
    }

    #[test]
    fn reasons_fit_the_notice() {
        for e in [
            P5zError::Image("x"),
            P5zError::Header("x"),
            P5zError::ArchiveId,
            P5zError::Region(0),
            P5zError::Volume(0),
        ] {
            assert!(!e.reason().is_empty() && e.reason().len() < 40);
            assert!(e.reason().is_ascii());
        }
    }
}
