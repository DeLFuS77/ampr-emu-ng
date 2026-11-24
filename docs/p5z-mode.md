# p5z mode (`maxpack.image`)

A `.p5z` is the single-file archive written by MaxPack. It stores each zstd
volume (`ampr_zstd-NNN.pak`) raw, byte for byte, in one region of the file.
In p5z mode the PS5 does not need the `.pak` files: ampr-emu-ng reads each
volume from its region inside the `.p5z`. Everything else of the zstd pack
convention is unchanged (`docs/zstd-format.md`): `ampr_zstd.index` and
`ampr_emu.index` are still loose files in `/app0`, and the chunk offsets of
the index are still offsets inside a volume.

The pure logic (parsing, header and region checks, offsets) is
`crates/ampr-format/src/p5z.rs`. It allocates nothing. Tests:
`crates/ampr-format/tests/p5z_fixture.rs`, with a real `.p5z` in
`crates/ampr-format/tests/fixtures/p5z/`.

## `maxpack.image`

`/app0/maxpack.image` tells where the regions are. The payload that mounts
the game writes it; ampr-emu-ng reads it. It is ASCII text, lines end with
`\n` (never `\r`), at most 12 KiB:

1. `maxpack-image 1`
2. The path of the `.p5z`, relative to `/app0`: `p5z/<file name>`. No
   leading `/`, no `\`, and no empty, `.` or `..` component.
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

that is `"maxpack-image 1\np5z/basic.p5z\n0 1234 5678\n"`. Anything else is
refused: another first line, a missing or unsafe path, ids that do not
start at 0 or skip or repeat or come out of order, no volumes or more than
256, numbers with a sign, letters or more than 64 bits, `\r`, an extra
line, or a file over 12 KiB.

Offsets are `u64`: the `.p5z` of a large game is well over 4 GiB, and every
sum of offsets is checked, never wrapping.

## Mode order

The loader picks the pack mode in this order, the first that applies:
**pkg, p5z, zstd, lz4, plain**. p5z comes right after pkg and before zstd
because a p5z game also has `ampr_zstd.index` in `/app0`, so it would
otherwise be taken for a plain zstd pack whose `.pak` files are missing.

## Checks at start-up

When `/app0/maxpack.image` exists the module checks everything before it
serves anything, so a damaged setup fails at once and not in the middle of
a game:

1. `maxpack.image` parses (above).
2. `ampr_zstd.index` opens (its CRCs included).
3. The first 64 bytes of the `.p5z` are a header: magic `MAXPKP5Z`, version
   1, header size 64, CRC-32 (IEEE) at bytes 60..64 over the 64 bytes with
   that field taken as zero.
4. The `archive_id` in the header (bytes 24..40) is the one of
   `ampr_zstd.index`.
5. For every volume of the index: the region is as long as the index says
   (`length == volume size`), starts at or after byte 64, and ends before
   the 32-byte footer at the end of the `.p5z` (no overflow).
6. The first 64 bytes of each region are that volume's header (id, count,
   archive id and size match the index).

## When a check fails

There is no fallback. The module does not switch to zstd or plain on its
own: the `.pak` files are not there, and reading another mode's files could
serve the wrong data. Every read of a packed file answers `EIO`, and the
notice (96 bytes at most) says why:

```
ampr-emu-ng: p5z: <reason>; packed files not served
```

`<reason>` is the text of `P5zError::reason`: `bad maxpack.image`,
`bad .p5z header`, `archive id mismatch`, `volume region out of range` or
`bad volume header`. When everything passes, the notice is the usual one
with the mode:

```
ampr-emu-ng: v<version> (p5z) running
```

(The start-up code is not in this crate: it is a later change in
`ampr-sys`, which will correct this page if it differs.)
