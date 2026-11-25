# p5z mode (`maxpack.image`)

A `.p5z` is the single-file archive written by MaxPack. It stores each zstd
volume (`ampr_zstd-NNN.pak`) raw, byte for byte, in one region of the file.
In p5z mode the PS5 does not need the `.pak` files: ampr-emu-ng reads each
volume from its region inside the `.p5z`. Everything else of the zstd pack
convention is unchanged (`docs/zstd-format.md`): `ampr_zstd.index` and
`ampr_emu.index` are still loose files in `/app0`, and the chunk offsets of
the index are still offsets inside a volume.

The pure logic (parsing, header and region checks, offsets) is
`crates/ampr-format/src/p5z.rs`, plus `header_volumes` and
`volume_header_for` in `crates/ampr-format/src/zstd.rs` for the index
header and the volume headers before the index is mounted. It allocates
nothing. Tests: `crates/ampr-format/tests/p5z_fixture.rs`, with real
`.p5z` files made by the MaxPack engine: one volume in
`crates/ampr-format/tests/fixtures/p5z/` and seven volumes in
`crates/ampr-format/tests/fixtures/p5z-multi/` (as in any game over about
4 GiB of packed data, every volume but the first starts past volume 0).
The runtime side (the start-up check, the region table, the reads) is
`crates/ampr-sys/src/rt/pack/p5z.rs`, whose tests run the check on the
same fixture files and on broken copies of them.

The payload that mounts the game writes `maxpack.image` into `/app0` and
mounts the folder that holds the `.p5z` read-only at `/app0/p5z/`. That
folder is the user's, and may hold other games' `.p5z` files, so in p5z
mode the index walk skips `/app0/p5z` (and everything under it) and
`/app0/maxpack.image`, and lookups there go to the filesystem, as pkg mode
does for `/app0/kraken`. A title without `maxpack.image` keeps a `p5z`
folder of its own as any other folder.

## `maxpack.image`

`/app0/maxpack.image` tells where the regions are. The payload that mounts
the game writes it; ampr-emu-ng reads it. It is UTF-8 text, lines end with
`\n` (never `\r`), with no other control byte (below 0x20, or 0x7F), at
most 12 KiB:

1. `maxpack-image 1`
2. The path of the `.p5z`, relative to `/app0`: `p5z/<name>`, where
   `<name>` is the file name of the `.p5z`: valid UTF-8, not empty, not
   `.` or `..`, with no byte below 0x20, no 0x7F, no `/` and no `\`. The
   name is the bytes the payload read from the directory, and the PS5 opens
   `/app0/p5z/<name>` with those same bytes, so any name the console's
   filesystem gives in UTF-8 round-trips (MaxPack's PC app names a `.p5z`
   `<Title> [<TITLE_ID>].p5z` and keeps every character of the title).
3. One line per volume, `<id> <start> <length>`: three decimal numbers
   separated by one space. `start` is the byte offset of the volume in the
   `.p5z`; `length` is its size in bytes, header included. The ids go 0 to
   n-1 in order, and n is between 1 and 256.

The final `\n` is optional when reading; the payload always writes it.
Reference text (a test on each side compares against it as is):

```
maxpack-image 1
p5z/basic.p5z
0 1234 5678
```

that is `"maxpack-image 1\np5z/basic.p5z\n0 1234 5678\n"`. A second test on
each side checks a UTF-8 name, the payload writing and ampr-emu-ng reading
exactly
`"maxpack-image 1\np5z/Starfall’s Odyssey™ 2 [PPSA01234].p5z\n0 1234 5678\n"`.
Anything else is refused: text that is not UTF-8, another first line, a
missing path, a path that is not `p5z/` and one name as above, ids that do
not start at 0 or skip or repeat or come out of order, no volumes or more
than 256, numbers with a sign, letters or more than 64 bits, `\r` or any
other control byte, an extra line, or a file over 12 KiB.

Offsets are `u64`: the `.p5z` of a large game is well over 4 GiB, and every
sum of offsets is checked, never wrapping.

## Mode order

The loader picks the pack mode in this order, the first that applies:
**pkg, p5z, zstd, lz4, plain**. p5z applies when `/app0/maxpack.image`
exists (a stat at module_start, before the hooks; a title without it pays
that one failed stat and nothing else changes). It comes right after pkg
and before zstd because a p5z game also has `ampr_zstd.index` in `/app0`,
so it would otherwise be taken for a plain zstd pack whose `.pak` files
are missing. A title with both `kraken.image` and `maxpack.image` is a pkg
title, as a pkg title with a zstd index is.

p5z is not a format of its own: the format stays zstd (`PackFormat::Zstd`),
so the index, the mount, the caches and the decoder are the zstd ones.
Only the place a volume is read from changes, and the mode label, which
the notice, the debug `/metrics` (`pack_format`) and the black box show,
is `p5z`.

## Checks at start-up

When `/app0/maxpack.image` exists the module checks everything before it
serves anything, so a damaged setup fails at once and not in the middle of
a game. The check runs in module_start before the hooks install, through
the unhooked kernel calls (as pkg mode reads `kraken.image`), in this
order, and stops at the first failure:

1. `maxpack.image` is a regular file of at most 12 KiB and parses (above).
   The path, with `/app0/` in front, must fit 265 bytes.
2. The header of `ampr_zstd.index` (its first 128 bytes), with every check
   of the mount that needs only those bytes, the same code
   (`header_volumes`): magic `AMPRZPK1`, version 1, header size 128, chunk
   shift, zstd level, reserved fields, the header CRC, the counts and the
   table layout. Its volume count must be the number of volume lines in
   `maxpack.image`.
3. The `.p5z` opens and is a regular file; its first 64 bytes are a header:
   magic `MAXPKP5Z`, version 1, header size 64, CRC-32 (IEEE) at bytes
   60..64 over the 64 bytes with that field taken as zero.
4. The `archive_id` in the header (bytes 24..40) is the one of
   `ampr_zstd.index` (bytes 24..40 of its header).
5. For every volume: its size is read from the volume table of the index
   (`volumes_off`, byte 72 of the header), and the region is as long as
   that size (`length == volume size`), starts at or after byte 64, and
   ends before the 32-byte footer at the end of the `.p5z` (no overflow).
6. The first 64 bytes of each region are that volume's header: the
   `ampr_format::zstd::volume_header` checks (id, archive id and size
   match the index, and the volume count is at most the index's: see
   [Growing a `.p5z`](#growing-a-p5z)), here against the values read in 2
   and 5 (`volume_header_for`, p5z rules).

The rest of `ampr_zstd.index` (its payload CRC, that its strings end the
file, and every record) is checked when it is mounted, right after, still in module_start, as for any
zstd title, with p5z rules (`Index::open_p5z`): if that fails no packed
file is served, as in zstd mode, and
the notice already sent is the usual one. Each volume's first open checks
its region and header again, against the mounted index and the size of
the open `.p5z`, as a `.pak` is checked when it opens.

## Growing a `.p5z`

MaxPack can add files to a `.p5z` without packing it again (a DLC that
comes out later, or the files a game update changes) by appending to it:
new volumes, numbered after the old ones and with the same `archive_id`,
then a new `ampr_zstd.index` and `ampr_emu.index`. Nothing before them is
written again, so two rules of a zstd pack cannot hold, and in p5z mode,
only there, ampr-emu-ng checks these instead (`Rules::P5z` in
`crates/ampr-format/src/zstd.rs`):

- **Where the chunks sit.** A zstd pack fills each volume from byte 64
  in record order, back to back. The new index sorts its records by path
  hash again, old files and new together, so their chunks no longer come
  in that order, and the files an update changed or removed leave room in
  the old volumes that no chunk uses any more (a volume can end up with no
  chunk at all). In p5z mode the chunks of each volume may sit in any order
  and with room between them, as long as no two overlap and each one lies
  inside `[64, size)` of its volume. A read is still bounded by its own
  chunk record, so the unused room is never read.
- **The volume count in a volume header.** A header written before the
  `.p5z` grew counts the volumes it had then. In p5z mode a header's count
  may be lower than the index's, never higher; its id, archive id and
  size must still match.

Every other check is the same: codec, sizes, store-only, volume ids below
the count, the sums and the CRCs. The overlap check sorts a copy of
the chunk records (8 B per chunk; an in-place heapsort with no
recursion, `ampr_format::sort::sort_u64`, which skips keys already in
order) in the chunk part of the block that then holds the chunk and
volume tables, taken before the open and filled
after it, so p5z mode needs no Index room beyond those tables; an index
whose tables do not fit is refused as in zstd mode. Without
`maxpack.image` nothing changes: a zstd pack is checked by the rules in
`docs/zstd-format.md`.

## Reads

Every volume is read from the `.p5z`: each volume opens it as its own fd
(the pack fd cache and its 32-fd cap are the zstd ones). The read ranges,
the windows and the chunk offsets stay offsets inside the volume; only the
kernel read adds the volume's region start (`PackHandle.base`, 0 for a
volume that is its own file). The sum is checked and must fit `off_t`; the
region checks above already keep it inside the `.p5z`. It is added once,
where a window's read request is built (`prepare_exact_window_aio` in
`rt/pack/window_aio.rs`); the pread pool (up to 8 threads) and kernel AIO
both take that request as it is, and lane EOP overlap
(`rt/reactor/lane_overlap.rs`) changes only when a command buffer's reads
are issued, not where they read. The volume header is read at the region
start too, at start-up and at each open (`validate_data_header_zstd` in
`rt/pack/leases.rs`), and the debug `/bench` adds the same base.

## When a check fails

There is no fallback. The module does not switch to zstd or plain on its
own: the `.pak` files are not there, and reading another mode's files could
serve the wrong data. The format stays zstd, every read of a packed file
answers `EIO` (no volume is opened), and the notice (96 bytes at most)
says why, in place of the usual one:

```
ampr-emu-ng: p5z: <reason>; packed files not served
```

`<reason>` is the text of `P5zError::reason` for the checks of
`ampr-format`: `bad maxpack.image` (1, or a volume count that is not the
index's), `bad .p5z header` (3, or a `.p5z` shorter than 64 bytes),
`archive id mismatch` (4), `volume region out of range` (5) or
`bad volume header` (6). The runtime adds two: `bad ampr_zstd.index` (the
index does not open, its header is refused or its volume table is short)
and `cannot open the .p5z` (the `.p5z` does not open or is not a regular
file). When everything passes, the notice is the usual one with the mode:

```
ampr-emu-ng: v<version> (p5z) running
```

The debug build writes how the check ended to the black box
(`/data/ampr_emu_trace.txt`): `p5z <how> <volume> <path> <rule>`, then
`mode <label>`. When the mount then refuses `ampr_zstd.index`, which
the notice already sent cannot say, it also writes `p5z mount <how>
<volume>`: `overlap` or `past-end` with the volume they are in (the
rules of [Growing a `.p5z`](#growing-a-p5z)), `crc` (the payload CRC),
`read` (a read error), `invalid` (any other index check) or `tables`
(no room for the chunk and volume tables). The release build writes
nothing more: as in zstd mode, no packed file is served.
