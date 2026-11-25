//! p5z mode: a zstd-pack title whose volumes are stored raw inside one
//! `.p5z` file and read from there (docs/p5z-mode.md). The payload that
//! mounts the game writes `/app0/maxpack.image` (the .p5z path and the byte
//! region of each volume) and mounts the folder that holds the .p5z
//! read-only at `/app0/p5z/`; `ampr_zstd.index` and the loose files stay in
//! /app0. The format stays `PackFormat::Zstd`: p5z changes only where the
//! bytes of a volume come from (leases.rs `build_pack_path`,
//! `validate_data_header_zstd`, `acquire_pack_fd`; window_aio.rs). This
//! holds the start-up check and the region table; parsing and the checks
//! are in `ampr_format::p5z` so host tests cover them.

use super::*;
use ampr_format::p5z::{
    check_header, check_region, parse_image, P5zError, Region, IMAGE_MAX_BYTES, MAX_VOLUMES,
};
use ampr_format::zstd::{
    header_volumes, volume_header_for, HeaderVolumes, Rules, INDEX_HEADER_SIZE,
    VOLUME_HEADER_SIZE, VOLUME_RECORD_SIZE,
};
use core::ptr::copy_nonoverlapping;

// ---- files ----

/// Present only in a p5z title: `select_pack_format` checks for it after
/// pkg and before the zstd index.
pub(super) const P5Z_IMAGE: &CStr = c"/app0/maxpack.image";
const _: () = assert!(zstd::ends_with(
    P5Z_IMAGE.to_bytes(),
    ampr_format::p5z::IMAGE_NAME.as_bytes()
));
const APP0_PREFIX: &[u8] = b"/app0/";
/// `/app0/p5z/`, a 255-byte name (d_namlen is a u8) and the NUL, as
/// `KRAKEN_PKG_PATH_MAX` in pkg.rs.
const P5Z_PATH_MAX: usize = b"/app0/p5z/".len() + 256;
/// The .p5z header is 64 bytes, as a volume header.
const P5Z_HEADER_SIZE: usize = 64;
const _: () = assert!(VOLUME_RECORD_SIZE == 8 && VOLUME_HEADER_SIZE == P5Z_HEADER_SIZE);

// ---- check result ----

/// Why the start-up check failed; the notice says `reason`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum P5zFail {
    /// A rule of `ampr_format::p5z`: maxpack.image, the .p5z header, a
    /// volume region or a volume header (`P5zError::Volume`).
    Check(P5zError),
    /// `ampr_zstd.index` did not open or read, its header was refused
    /// (`header_volumes`), or its volume table is short.
    Index,
    /// The .p5z that maxpack.image names did not open or is not a regular
    /// file.
    Open,
}

/// Longest `P5zFail::reason`, for the notice line (exports.rs).
pub(crate) const P5Z_REASON_MAX: usize = 40;

impl P5zFail {
    /// Short English text for the notice (`ampr-emu-ng: p5z: <reason>; ...`).
    pub(crate) fn reason(self) -> &'static [u8] {
        match self {
            Self::Check(e) => e.reason(),
            Self::Index => b"bad ampr_zstd.index",
            Self::Open => b"cannot open the .p5z",
        }
    }
}

/// What a passed check keeps for the reads.
pub(super) struct P5zTable {
    /// NUL-terminated `/app0/<path in maxpack.image>`.
    path: [u8; P5Z_PATH_MAX],
    /// The region of volume k in the .p5z, for k below `count`.
    regions: [Region; MAX_VOLUMES],
    /// Volumes checked: 0 until the whole check passes.
    count: usize,
}

impl P5zTable {
    const EMPTY: Self = Self {
        path: [0; P5Z_PATH_MAX],
        regions: [Region { start: 0, len: 0 }; MAX_VOLUMES],
        count: 0,
    };
}

const OFF: u8 = 0;
const GOOD: u8 = 1;
const BAD: u8 = 2;

/// OFF (no maxpack.image), GOOD or BAD. The Release store at the end of
/// `discover_p5z` publishes TABLE and FAILURE; nothing writes them after.
static STATE: AtomicU8 = AtomicU8::new(OFF);
/// .bss outside the pool: 4,376 B (266 B of path, 256 regions of 16 B,
/// the count; `p5z_statics_keep_their_size`).
static TABLE: SyncCell<P5zTable> = SyncCell::new(P5zTable::EMPTY);
static FAILURE: SyncCell<Option<P5zFail>> = SyncCell::new(None);
/// maxpack.image, read whole for `parse_image` at module_start only. In
/// .bss rather than on the stack: 12 KiB would be the largest frame in the
/// PRX, on a thread whose stack the game sized.
static TEXT: SyncCell<[u8; IMAGE_MAX_BYTES]> = SyncCell::new([0; IMAGE_MAX_BYTES]);

#[inline]
fn state() -> u8 {
    STATE.load(Ordering::Acquire)
}

/// True when `/app0/maxpack.image` was there at module_start: the volumes
/// are in a .p5z, whether its check passed or not.
pub fn p5z_active() -> bool {
    state() != OFF
}

/// The failed check, when maxpack.image is there and the check failed: no
/// packed file is served (`acquire_pack_fd` answers EIO).
pub(crate) fn p5z_failure() -> Option<P5zFail> {
    if state() != BAD {
        return None;
    }
    // Written before the Release store `state` acquired, never after.
    unsafe { *FAILURE.get() }
}

/// Region of volume `pack` in the .p5z, once the check passed.
pub fn p5z_region(pack: u32) -> Option<Region> {
    if state() != GOOD {
        return None;
    }
    // Written before the Release store `state` acquired, never after.
    let table = unsafe { &*TABLE.get() };
    table.regions.get(..table.count)?.get(pack as usize).copied()
}

/// File offset of volume `pack`'s byte 0: its region start in p5z mode,
/// else 0 (the volume is its own file).
pub(crate) fn pack_base(pack: u16) -> u64 {
    p5z_region(u32::from(pack)).map_or(0, |r| r.start)
}

/// The .p5z path with its NUL; empty unless the check passed.
fn p5z_path() -> &'static [u8] {
    if state() != GOOD {
        return &[];
    }
    table_path()
}

/// The path maxpack.image named, with its NUL, once the check ran (empty
/// when it failed before the path): for the black box.
fn table_path() -> &'static [u8] {
    if state() == OFF {
        return &[];
    }
    // Written before the Release store `state` acquired, never after.
    let path = unsafe { &(*TABLE.get()).path };
    match path.iter().position(|&b| b == 0) {
        Some(n) if n > 0 => &path[..=n],
        _ => &[],
    }
}

// ---- the start-up check ----

/// The calls the check makes, so host tests serve the files from memory.
pub(super) trait P5zDisk {
    /// Opens `path` read-only; None on any error.
    fn open(&mut self, path: &CStr) -> Option<c_int>;
    /// Size of `fd` when it is a regular file.
    fn regular_size(&mut self, fd: c_int) -> Option<u64>;
    /// Fills `buf` from `off`; false on an error or a short file.
    fn pread_full(&mut self, fd: c_int, buf: &mut [u8], off: u64) -> bool;
    fn close(&mut self, fd: c_int);
}

/// The unhooked kernel calls of pkg.rs discovery, valid at module_start
/// before the hooks install.
struct Kernel;

impl P5zDisk for Kernel {
    fn open(&mut self, path: &CStr) -> Option<c_int> {
        let fd = unsafe { open_ro(path.as_ptr()) };
        (fd >= 0).then_some(fd)
    }
    fn regular_size(&mut self, fd: c_int) -> Option<u64> {
        unsafe { regular_size(fd) }
    }
    fn pread_full(&mut self, fd: c_int, buf: &mut [u8], off: u64) -> bool {
        unsafe { pread_full(fd, buf, off) }
    }
    fn close(&mut self, fd: c_int) {
        let _ = unsafe { sys::real_physical_close(fd) };
    }
}

/// The start-up check of docs/p5z-mode.md, in order: maxpack.image, the
/// index header, the .p5z header, then for each volume its region and its
/// 64-byte header. Every fd it opens is closed before it returns. On
/// success `table` holds the .p5z path and `count` regions; on failure
/// `count` is 0, so no region is ever served from a failed check.
pub(super) fn check_p5z<D: P5zDisk>(
    d: &mut D,
    text: &mut [u8; IMAGE_MAX_BYTES],
    table: &mut P5zTable,
) -> Result<(), P5zFail> {
    table.count = 0;
    let n = read_image(d, text, table)?;
    let index = d.open(zstd::AMPR_EMU_ZSTD_INDEX_PATH).ok_or(P5zFail::Index)?;
    let result = check_with_index(d, index, n, table);
    d.close(index);
    result?;
    table.count = n;
    Ok(())
}

/// Reads and parses maxpack.image into `table` (path and regions); the
/// volume count.
fn read_image<D: P5zDisk>(
    d: &mut D,
    text: &mut [u8; IMAGE_MAX_BYTES],
    table: &mut P5zTable,
) -> Result<usize, P5zFail> {
    let image = |rule| P5zFail::Check(P5zError::Image(rule));
    let fd = d.open(P5Z_IMAGE).ok_or(image("open"))?;
    let read = read_whole(d, fd, text);
    d.close(fd);
    let (path, n) = parse_image(read.map_err(image)?, &mut table.regions).map_err(P5zFail::Check)?;
    // parse_image refuses NUL bytes, so the path ends at the NUL below.
    let end = APP0_PREFIX.len() + path.len();
    if end >= P5Z_PATH_MAX {
        return Err(image("path too long"));
    }
    table.path[..APP0_PREFIX.len()].copy_from_slice(APP0_PREFIX);
    table.path[APP0_PREFIX.len()..end].copy_from_slice(path);
    table.path[end] = 0;
    Ok(n)
}

/// The whole of regular file `fd` into `text`; a file over IMAGE_MAX_BYTES
/// is refused unread.
fn read_whole<'t, D: P5zDisk>(
    d: &mut D,
    fd: c_int,
    text: &'t mut [u8; IMAGE_MAX_BYTES],
) -> Result<&'t [u8], &'static str> {
    let size = d.regular_size(fd).ok_or("not a regular file")?;
    let len = usize::try_from(size)
        .ok()
        .filter(|&n| n <= IMAGE_MAX_BYTES)
        .ok_or("too long")?;
    let buf = &mut text[..len];
    if !d.pread_full(fd, buf, 0) {
        return Err("read");
    }
    Ok(buf)
}

/// The index header, the volume count against maxpack.image's, then the
/// .p5z (`check_volumes`).
fn check_with_index<D: P5zDisk>(
    d: &mut D,
    index: c_int,
    n: usize,
    table: &P5zTable,
) -> Result<(), P5zFail> {
    let mut h = [0u8; INDEX_HEADER_SIZE];
    if d.regular_size(index).is_none() || !d.pread_full(index, &mut h, 0) {
        return Err(P5zFail::Index);
    }
    let v = header_volumes(&h).map_err(|_| P5zFail::Index)?;
    if v.count as usize != n {
        return Err(P5zFail::Check(P5zError::Image("volume count is not the index's")));
    }
    let regions = table.regions.get(..n).ok_or(P5zFail::Index)?;
    let path = CStr::from_bytes_until_nul(&table.path).map_err(|_| P5zFail::Open)?;
    let fd = d.open(path).ok_or(P5zFail::Open)?;
    let result = check_volumes(d, index, fd, &v, regions);
    d.close(fd);
    result
}

/// The .p5z header, then each volume: its size from the index's volume
/// table, its region (`check_region`: the volume's size, past the header,
/// before the footer, no overflow) and the 64 bytes at the region start,
/// compared as `ampr_format::zstd::volume_header` compares them under p5z
/// rules (`Rules::P5z`): a header written before the .p5z grew counts
/// fewer volumes than the index.
fn check_volumes<D: P5zDisk>(
    d: &mut D,
    index: c_int,
    fd: c_int,
    v: &HeaderVolumes,
    regions: &[Region],
) -> Result<(), P5zFail> {
    let size = d.regular_size(fd).ok_or(P5zFail::Open)?;
    let mut h = [0u8; P5Z_HEADER_SIZE];
    if !d.pread_full(fd, &mut h, 0) {
        return Err(P5zFail::Check(P5zError::Header("short file")));
    }
    check_header(&h, &v.archive_id).map_err(P5zFail::Check)?;
    for (k, &region) in (0u32..).zip(regions) {
        let at = u64::from(k)
            .checked_mul(VOLUME_RECORD_SIZE as u64)
            .and_then(|o| v.volumes_at.checked_add(o))
            .ok_or(P5zFail::Index)?;
        let mut record = [0u8; VOLUME_RECORD_SIZE];
        if !d.pread_full(index, &mut record, at) {
            return Err(P5zFail::Index);
        }
        let volume_size = u64::from_le_bytes(record);
        check_region(k, region, size, volume_size).map_err(P5zFail::Check)?;
        if !d.pread_full(fd, &mut h, region.start)
            || volume_header_for(&h, k, v.count, &v.archive_id, volume_size, Rules::P5z).is_err()
        {
            return Err(P5zFail::Check(P5zError::Volume(k)));
        }
    }
    Ok(())
}

/// module_start, before the hooks (`select_pack_format`), once
/// `/app0/maxpack.image` exists: runs the check and publishes the result.
/// The title is a zstd one either way; after a failed check no packed file
/// is served and the notice says why, with no fall back to `.pak` files.
pub(super) unsafe fn discover_p5z() {
    // SAFETY: module_start is the only writer and runs before any reader
    // (hooks, runtime threads); the Release store below publishes it.
    let (text, table, failure) =
        unsafe { (&mut *TEXT.get(), &mut *TABLE.get(), &mut *FAILURE.get()) };
    let result = check_p5z(&mut Kernel, text, table);
    *failure = result.err();
    STATE.store(if result.is_ok() { GOOD } else { BAD }, Ordering::Release);
}

/// The black box line on how the check ended (debug build only):
/// "p5z <how> <volume> <path or -> <rule>".
#[cfg(feature = "mem_report")]
pub(crate) fn log_p5z_discovery() {
    let (how, volume, rule): (&[u8], u32, &str) = match (state(), p5z_failure()) {
        (OFF, _) => (b"off", 0, ""),
        (_, None) => (b"ok", 0, ""),
        (_, Some(P5zFail::Index)) => (b"index", 0, ""),
        (_, Some(P5zFail::Open)) => (b"open", 0, ""),
        (_, Some(P5zFail::Check(e))) => match e {
            P5zError::Image(rule) => (b"image", 0, rule),
            P5zError::Header(rule) => (b"header", 0, rule),
            P5zError::ArchiveId => (b"archiveid", 0, ""),
            P5zError::Region(k) => (b"region", k, ""),
            P5zError::Volume(k) => (b"volume", k, ""),
        },
    };
    crate::rt::blackbox::p5z(how, u64::from(volume), table_path(), rule.as_bytes());
}

// ---- pack volumes ----

/// leases.rs `build_pack_path`, p5z: every volume is read from the .p5z.
pub(super) unsafe fn build_pack_path_p5z(
    state: *const PackState,
    pack_id: u16,
    output: *mut c_char,
    output_size: usize,
) -> bool {
    let path = p5z_path();
    if output.is_null()
        || u32::from(pack_id) >= unsafe { pack_volume_count(state) }
        || p5z_region(u32::from(pack_id)).is_none()
        || path.is_empty()
        || path.len() > output_size
    {
        return false;
    }
    unsafe { copy_nonoverlapping(path.as_ptr(), output as *mut u8, path.len()) };
    true
}

/// leases.rs `validate_data_header_zstd`, p5z: where volume `volume`'s
/// header is in the .p5z of `file_size` bytes, after the start-up rule
/// (`check_region`) against the mounted index's `volume_size`. None when
/// the check failed or the region does not hold the volume.
pub(super) fn p5z_volume_start(volume: u32, file_size: u64, volume_size: u64) -> Option<u64> {
    let region = p5z_region(volume)?;
    check_region(volume, region, file_size, volume_size).ok()?;
    Some(region.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::format;
    use std::vec::Vec;

    // The real game of crates/ampr-format/tests/fixtures/p5z/README.md: one
    // volume of 2,067,009 bytes at 10,149 in a 2,094,371-byte .p5z.
    const P5Z: &[u8] = include_bytes!("../../../../ampr-format/tests/fixtures/p5z/basic.p5z");
    const INDEX: &[u8] =
        include_bytes!("../../../../ampr-format/tests/fixtures/p5z/ampr_zstd.index");
    const IMAGE: &[u8] = include_bytes!("../../../../ampr-format/tests/fixtures/p5z/maxpack.image");
    const GAME: &CStr = c"/app0/p5z/basic.p5z";
    const START: u64 = 10_149;
    const LEN: u64 = 2_067_009;

    /// Files by path, `regular` or not; fd = 100 + the file's position.
    struct Mem {
        files: Vec<(&'static CStr, Vec<u8>, bool)>,
        open: Vec<c_int>,
    }

    impl Mem {
        fn game() -> Self {
            Self {
                files: std::vec![
                    (P5Z_IMAGE, IMAGE.to_vec(), true),
                    (zstd::AMPR_EMU_ZSTD_INDEX_PATH, INDEX.to_vec(), true),
                    (GAME, P5Z.to_vec(), true),
                ],
                open: Vec::new(),
            }
        }
        fn file(&mut self, path: &CStr) -> &mut (&'static CStr, Vec<u8>, bool) {
            self.files.iter_mut().find(|f| f.0 == path).unwrap()
        }
        fn bytes(&mut self, path: &CStr) -> &mut Vec<u8> {
            &mut self.file(path).1
        }
        fn remove(&mut self, path: &CStr) {
            self.files.retain(|f| f.0 != path);
        }
        fn at(&self, fd: c_int) -> &(&'static CStr, Vec<u8>, bool) {
            assert!(self.open.contains(&fd), "fd {fd} not open");
            &self.files[(fd - 100) as usize]
        }
    }

    impl P5zDisk for Mem {
        fn open(&mut self, path: &CStr) -> Option<c_int> {
            let fd = 100 + self.files.iter().position(|f| f.0 == path)? as c_int;
            self.open.push(fd);
            Some(fd)
        }
        fn regular_size(&mut self, fd: c_int) -> Option<u64> {
            let f = self.at(fd);
            f.2.then_some(f.1.len() as u64)
        }
        fn pread_full(&mut self, fd: c_int, buf: &mut [u8], off: u64) -> bool {
            let data = &self.at(fd).1;
            let src = usize::try_from(off)
                .ok()
                .and_then(|o| Some(o..o.checked_add(buf.len())?))
                .and_then(|r| data.get(r));
            src.map(|s| buf.copy_from_slice(s)).is_some()
        }
        fn close(&mut self, fd: c_int) {
            let at = self.open.iter().position(|&f| f == fd).expect("closed once");
            self.open.remove(at);
        }
    }

    fn run(disk: &mut Mem) -> (Result<(), P5zFail>, Box<P5zTable>) {
        let mut text = Box::new([0u8; IMAGE_MAX_BYTES]);
        let mut table = Box::new(P5zTable::EMPTY);
        let result = check_p5z(disk, &mut text, &mut table);
        assert!(disk.open.is_empty(), "every fd closed: {:?}", disk.open);
        if result.is_err() {
            assert_eq!(table.count, 0, "a failed check keeps no region");
        }
        (result, table)
    }

    fn image(start: u64, len: u64) -> Vec<u8> {
        format!("maxpack-image 1\np5z/basic.p5z\n0 {start} {len}\n").into_bytes()
    }

    /// Recomputes the CRC at `crc_at` of the `size` bytes at `at`, taken
    /// with the CRC field as zero (.p5z and volume headers).
    fn reseal(bytes: &mut [u8], at: usize, size: usize, crc_at: usize) {
        let h = &mut bytes[at..at + size];
        h[crc_at..crc_at + 4].fill(0);
        let crc = ampr_format::crc32(h);
        h[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
    }

    /// Recomputes the index header CRC (116), keeping the payload CRC (112).
    fn reseal_index(bytes: &mut [u8]) {
        let payload: [u8; 4] = bytes[112..116].try_into().unwrap();
        bytes[112..120].fill(0);
        let crc = ampr_format::crc32(&bytes[..INDEX_HEADER_SIZE]);
        bytes[112..116].copy_from_slice(&payload);
        bytes[116..120].copy_from_slice(&crc.to_le_bytes());
    }

    #[test]
    fn p5z_check_passes_the_fixture_game() {
        assert_eq!(IMAGE, &image(START, LEN)[..]);
        let mut disk = Mem::game();
        let (result, table) = run(&mut disk);
        assert_eq!(result, Ok(()));
        assert_eq!(table.count, 1);
        assert_eq!(
            table.regions[0],
            Region {
                start: START,
                len: LEN
            }
        );
        let n = table.path.iter().position(|&b| b == 0).unwrap();
        assert_eq!(&table.path[..=n], GAME.to_bytes_with_nul());
        // Volume 0's chunks are the .pak's bytes at START (the fixture
        // test of ampr-format compares them).
        assert_eq!(P5Z.len() as u64, 2_094_371);
    }

    /// The check's error on the fixture game after `edit`.
    fn refused(edit: impl FnOnce(&mut Mem)) -> P5zFail {
        let mut disk = Mem::game();
        edit(&mut disk);
        run(&mut disk).0.expect_err("the check passed")
    }

    const INDEX_PATH: &CStr = zstd::AMPR_EMU_ZSTD_INDEX_PATH;

    #[test]
    fn p5z_check_refuses_a_bad_image() {
        let crlf = b"maxpack-image 1\r\np5z/basic.p5z\r\n0 10149 2067009\r\n";
        let two = b"maxpack-image 1\np5z/basic.p5z\n0 10149 2067009\n1 2077158 65\n";
        let long = format!(
            "maxpack-image 1\np5z/{}\n0 10149 2067009\n",
            "a".repeat(260)
        );
        for (what, fail) in [
            ("gone after the stat", refused(|d| d.remove(P5Z_IMAGE))),
            ("not a regular file", refused(|d| d.file(P5Z_IMAGE).2 = false)),
            ("CRLF line ends", refused(|d| *d.bytes(P5Z_IMAGE) = crlf.to_vec())),
            (
                "over 12 KiB",
                refused(|d| d.bytes(P5Z_IMAGE).resize(IMAGE_MAX_BYTES + 1, b'\n')),
            ),
            (
                "two volumes, the index has one",
                refused(|d| *d.bytes(P5Z_IMAGE) = two.to_vec()),
            ),
            (
                "path past the path buffer",
                refused(|d| *d.bytes(P5Z_IMAGE) = long.clone().into_bytes()),
            ),
        ] {
            assert!(
                matches!(fail, P5zFail::Check(P5zError::Image(_))),
                "{what}: {fail:?}"
            );
        }
    }

    #[test]
    fn p5z_check_refuses_a_bad_index() {
        assert_eq!(refused(|d| d.remove(INDEX_PATH)), P5zFail::Index);
        assert_eq!(refused(|d| d.bytes(INDEX_PATH).truncate(100)), P5zFail::Index);
        // Header CRC: an edit without a new CRC.
        assert_eq!(refused(|d| d.bytes(INDEX_PATH)[30] ^= 1), P5zFail::Index);
        // A volume table offset the header's own layout refuses (resealed).
        let far = |d: &mut Mem| {
            let ix = d.bytes(INDEX_PATH);
            ix[72..80].copy_from_slice(&(u64::MAX - 3).to_le_bytes());
            reseal_index(ix);
        };
        assert_eq!(refused(far), P5zFail::Index);
        // A good header in a file cut before its volume table: the read of
        // the table fails.
        let at = u64::from_le_bytes(INDEX[72..80].try_into().unwrap()) as usize;
        assert_eq!(refused(|d| d.bytes(INDEX_PATH).truncate(at)), P5zFail::Index);
    }

    #[test]
    fn p5z_check_refuses_a_bad_p5z() {
        use P5zError::*;
        assert_eq!(refused(|d| d.remove(GAME)), P5zFail::Open);
        assert_eq!(refused(|d| d.file(GAME).2 = false), P5zFail::Open);
        assert!(matches!(
            refused(|d| d.bytes(GAME).truncate(10)),
            P5zFail::Check(Header(_))
        ));
        let other_archive = |d: &mut Mem| {
            let p = d.bytes(GAME);
            p[30] ^= 0xff;
            reseal(p, 0, 64, 60);
        };
        assert_eq!(refused(other_archive), P5zFail::Check(ArchiveId));
        // Cut where the region ends: no room for the footer.
        let cut = |d: &mut Mem| d.bytes(GAME).truncate((START + LEN) as usize);
        assert_eq!(refused(cut), P5zFail::Check(Region(0)));
    }

    #[test]
    fn p5z_check_refuses_a_bad_region_or_volume_header() {
        use P5zError::*;
        let region = |start, len| refused(move |d| *d.bytes(P5Z_IMAGE) = image(start, len));
        assert_eq!(region(u64::MAX - 5, LEN), P5zFail::Check(Region(0)));
        assert_eq!(region(START, LEN - 1), P5zFail::Check(Region(0)));
        assert_eq!(region(START, LEN + 1), P5zFail::Check(Region(0)));
        assert_eq!(region(63, LEN), P5zFail::Check(Region(0)));
        // In the file, but one byte late: no volume header there.
        assert_eq!(region(START + 1, LEN), P5zFail::Check(Volume(0)));
        let other_archive = |d: &mut Mem| {
            let p = d.bytes(GAME);
            p[START as usize + 30] ^= 0xff;
            reseal(p, START as usize, 64, 60);
        };
        assert_eq!(refused(other_archive), P5zFail::Check(Volume(0)));
        let crc = |d: &mut Mem| d.bytes(GAME)[START as usize + 50] ^= 1;
        assert_eq!(refused(crc), P5zFail::Check(Volume(0)));
    }

    /// The name MaxPack's PC app gives a `.p5z` ("<Title> [<TITLE_ID>].p5z")
    /// keeps the title's ™, ’ and accents: the check opens it as it is.
    const UTF8_GAME: &CStr = c"/app0/p5z/Starfall’s Odyssey™ 2 [PPSA01234].p5z";

    #[test]
    fn p5z_check_passes_a_utf8_name() {
        let mut disk = Mem::game();
        let text = format!(
            "maxpack-image 1\np5z/Starfall’s Odyssey™ 2 [PPSA01234].p5z\n0 {START} {LEN}\n"
        );
        *disk.bytes(P5Z_IMAGE) = text.into_bytes();
        disk.file(GAME).0 = UTF8_GAME;
        let (result, table) = run(&mut disk);
        assert_eq!(result, Ok(()));
        let n = table.path.iter().position(|&b| b == 0).unwrap();
        assert_eq!(&table.path[..=n], UTF8_GAME.to_bytes_with_nul());
        // The same name with a control byte, or not UTF-8, is refused.
        for name in [&b"a\x01b.p5z"[..], b"a\xffb.p5z", b"a\\b.p5z"] {
            let mut text = b"maxpack-image 1\np5z/".to_vec();
            text.extend_from_slice(name);
            text.extend_from_slice(format!("\n0 {START} {LEN}\n").as_bytes());
            let fail = refused(|d| *d.bytes(P5Z_IMAGE) = text);
            assert!(
                matches!(fail, P5zFail::Check(P5zError::Image(_))),
                "{name:?}: {fail:?}"
            );
        }
    }

    // The seven-volume game of
    // crates/ampr-format/tests/fixtures/p5z-multi/README.md: every volume
    // but the first starts past volume 0, as in any game over about 4 GiB
    // of packed data.
    const M_P5Z: &[u8] =
        include_bytes!("../../../../ampr-format/tests/fixtures/p5z-multi/multi.p5z");
    const M_INDEX: &[u8] =
        include_bytes!("../../../../ampr-format/tests/fixtures/p5z-multi/ampr_zstd.index");
    const M_IMAGE: &[u8] =
        include_bytes!("../../../../ampr-format/tests/fixtures/p5z-multi/maxpack.image");
    const M_GAME: &CStr = c"/app0/p5z/multi.p5z";
    /// (start, length) of each volume, from `info --play`.
    const M_REGIONS: [(u64, u64); 7] = [
        (10_149, 264_799),
        (274_948, 262_208),
        (537_156, 265_802),
        (802_958, 262_208),
        (1_065_166, 442_557),
        (1_507_723, 286_971),
        (1_794_694, 282_848),
    ];

    impl Mem {
        fn multi() -> Self {
            Self {
                files: std::vec![
                    (P5Z_IMAGE, M_IMAGE.to_vec(), true),
                    (zstd::AMPR_EMU_ZSTD_INDEX_PATH, M_INDEX.to_vec(), true),
                    (M_GAME, M_P5Z.to_vec(), true),
                ],
                open: Vec::new(),
            }
        }
    }

    fn m_image(regions: &[(u64, u64)]) -> Vec<u8> {
        let mut s = std::string::String::from("maxpack-image 1\np5z/multi.p5z\n");
        for (k, (start, len)) in regions.iter().enumerate() {
            s.push_str(&format!("{k} {start} {len}\n"));
        }
        s.into_bytes()
    }

    /// The check's error on the seven-volume game after `edit`.
    fn refused_multi(edit: impl FnOnce(&mut Mem)) -> P5zFail {
        let mut disk = Mem::multi();
        edit(&mut disk);
        run(&mut disk).0.expect_err("the check passed")
    }

    #[test]
    fn p5z_check_passes_the_multi_volume_game() {
        assert_eq!(M_IMAGE, &m_image(&M_REGIONS)[..]);
        let mut disk = Mem::multi();
        let (result, table) = run(&mut disk);
        assert_eq!(result, Ok(()));
        assert_eq!(table.count, M_REGIONS.len());
        for (k, &(start, len)) in M_REGIONS.iter().enumerate() {
            assert_eq!(table.regions[k], Region { start, len }, "volume {k}");
        }
        let n = table.path.iter().position(|&b| b == 0).unwrap();
        assert_eq!(&table.path[..=n], M_GAME.to_bytes_with_nul());
        assert_eq!(M_P5Z.len() as u64, 2_095_040);
    }

    /// Broken copies of the seven-volume game where only volume k >= 1 is
    /// wrong: the check reads volume k's size at `volumes_at + 8k` and
    /// names volume k.
    #[test]
    fn p5z_check_refuses_a_bad_later_volume() {
        use P5zError::*;
        let volumes_at = u64::from_le_bytes(M_INDEX[72..80].try_into().unwrap()) as usize;
        for k in 1..M_REGIONS.len() {
            let id = k as u32;
            let (start, len) = M_REGIONS[k];
            let with = |start: u64, len: u64| {
                let mut r = M_REGIONS;
                r[k] = (start, len);
                m_image(&r)
            };
            let image = |text: Vec<u8>| refused_multi(move |d| *d.bytes(P5Z_IMAGE) = text);
            assert_eq!(image(with(start, len - 1)), P5zFail::Check(Region(id)), "{k}");
            assert_eq!(image(with(start, len + 1)), P5zFail::Check(Region(id)), "{k}");
            // In the file, but one byte late: no volume header there.
            assert_eq!(image(with(start + 1, len)), P5zFail::Check(Volume(id)), "{k}");
            // Volume 0's region in place of volume k's.
            let (start0, len0) = M_REGIONS[0];
            let want = if len0 == len { Volume(id) } else { Region(id) };
            assert_eq!(image(with(start0, len0)), P5zFail::Check(want), "{k}");

            // Volume k's header in the .p5z: another archive (resealed), or
            // its CRC.
            let at = start as usize;
            let other_archive = move |d: &mut Mem| {
                let p = d.bytes(M_GAME);
                p[at + 30] ^= 0xff;
                reseal(p, at, 64, 60);
            };
            assert_eq!(refused_multi(other_archive), P5zFail::Check(Volume(id)), "{k}");
            let crc = move |d: &mut Mem| d.bytes(M_GAME)[at + 50] ^= 1;
            assert_eq!(refused_multi(crc), P5zFail::Check(Volume(id)), "{k}");

            // Volume k's size in the index's volume table (the start-up
            // check does not read the payload CRC that covers it).
            let record = volumes_at + 8 * k;
            let bigger = move |d: &mut Mem| {
                let ix = d.bytes(INDEX_PATH);
                let size = u64::from_le_bytes(ix[record..record + 8].try_into().unwrap());
                ix[record..record + 8].copy_from_slice(&(size + 1).to_le_bytes());
            };
            assert_eq!(refused_multi(bigger), P5zFail::Check(Region(id)), "{k}");
            // An index cut inside volume k's size.
            let cut = move |d: &mut Mem| d.bytes(INDEX_PATH).truncate(record + 4);
            assert_eq!(refused_multi(cut), P5zFail::Index, "{k}");
        }

        // Volumes 1 and 3 have the same size: swapped, both regions fit and
        // the header at volume 1's start is volume 3's.
        assert_eq!(M_REGIONS[1].1, M_REGIONS[3].1);
        let mut swapped = M_REGIONS;
        swapped.swap(1, 3);
        let text = m_image(&swapped);
        assert_eq!(
            refused_multi(move |d| *d.bytes(P5Z_IMAGE) = text),
            P5zFail::Check(Volume(1))
        );
        // Cut so the footer would overlap the last region by one byte.
        let (start6, len6) = M_REGIONS[6];
        let size = (start6 + len6 + 31) as usize;
        assert_eq!(
            refused_multi(move |d| d.bytes(M_GAME).truncate(size)),
            P5zFail::Check(Region(6))
        );
        // One volume line fewer than the index has.
        let text = m_image(&M_REGIONS[..6]);
        assert!(matches!(
            refused_multi(move |d| *d.bytes(P5Z_IMAGE) = text),
            P5zFail::Check(Image(_))
        ));
    }

    /// A volume header written before the .p5z grew counts the volumes the
    /// .p5z had then: the start-up check takes it (p5z rules), one volume
    /// at a time or all at once, but not a header that counts more volumes
    /// than the index.
    #[test]
    fn p5z_check_takes_volume_headers_written_before_the_p5z_grew() {
        use P5zError::*;
        let with_count = |k: usize, count: u32| {
            let at = M_REGIONS[k].0 as usize;
            move |d: &mut Mem| {
                let p = d.bytes(M_GAME);
                p[at + 20..at + 24].copy_from_slice(&count.to_le_bytes());
                reseal(p, at, 64, 60);
            }
        };
        let volumes = M_REGIONS.len();
        for k in 0..volumes {
            let mut disk = Mem::multi();
            with_count(k, k as u32 + 1)(&mut disk);
            assert_eq!(run(&mut disk).0, Ok(()), "volume {k}");
            let over = refused_multi(with_count(k, volumes as u32 + 1));
            assert_eq!(over, P5zFail::Check(Volume(k as u32)), "volume {k}");
        }
        let mut disk = Mem::multi();
        for k in 0..volumes {
            with_count(k, k as u32 + 1)(&mut disk);
        }
        assert_eq!(run(&mut disk).0, Ok(()));
    }

    /// Every failure text fits the notice line (exports.rs `NOTICE_MAX`).
    #[test]
    fn p5z_reasons_are_short_ascii() {
        for f in [
            P5zFail::Check(P5zError::Image("x")),
            P5zFail::Check(P5zError::Header("x")),
            P5zFail::Check(P5zError::ArchiveId),
            P5zFail::Check(P5zError::Region(0)),
            P5zFail::Check(P5zError::Volume(0)),
            P5zFail::Index,
            P5zFail::Open,
        ] {
            let r = f.reason();
            assert!(!r.is_empty() && r.len() <= P5Z_REASON_MAX && r.is_ascii(), "{f:?}");
        }
    }

    /// The statics are .bss outside the pool, so their size feeds the pool
    /// recompute (DESIGN.md memory budget), as `cache_entries_are_56_bytes`.
    #[test]
    fn p5z_statics_keep_their_size() {
        assert_eq!(core::mem::size_of::<P5zTable>(), 4376);
        assert_eq!(core::mem::size_of::<Option<P5zFail>>(), 24);
        // 40 B before `base`.
        assert_eq!(core::mem::size_of::<PackHandle>(), 48);
    }

    /// The host never runs discover_p5z: no maxpack.image, nothing served
    /// from a .p5z, every pack at base 0.
    #[test]
    fn p5z_is_off_on_the_host() {
        assert!(!p5z_active());
        assert_eq!(p5z_failure(), None);
        assert_eq!(p5z_region(0), None);
        assert_eq!(pack_base(0), 0);
        assert!(p5z_path().is_empty());
        assert_eq!(p5z_volume_start(0, 1 << 30, 1 << 20), None);
    }
}
