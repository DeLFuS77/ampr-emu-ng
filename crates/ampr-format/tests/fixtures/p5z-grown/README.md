# p5z fixture that grew by appending

A small `.p5z` that the MaxPack engine grew twice by appending, as
`maxpack update` and `maxpack add` leave one (`.p5z` index version 2):
the chunks of `ampr_zstd.index` out of record order, room no chunk uses in
the old volumes, a volume with no chunk at all, and volume headers written
before it grew that count fewer volumes than the index. Only p5z rules
take it (`docs/p5z-mode.md`, "Growing a `.p5z`"). Its files are synthetic
test data.

Made with the MaxPack engine CLI (maxpack 1.0.6, branch `pc-ampliar`) by
`make_grown.py` (here, as it ran), from the engine's own test game and DLC
(its `tests/tools/make_images.py` and `make_fpkg.py`):

    maxpack pack game -o grown.p5z --p5z --volume-limit 262208
    maxpack update grown.p5z v2 --force --volume-limit 262208
    maxpack add grown.p5z --dlc dlc_data --volume-limit 262208
    maxpack verify grown.p5z
    maxpack info --play grown.p5z

`v2` is the game with `sce_sys/param.json` at content version
01.001.000, `data/big.bin` changed (same size), `data/noise.bin` removed
and `data/new_v2.bin` new (301,000 bytes; the README.txt the script
writes says 308,000, which is wrong). `--volume-limit` (for tests) caps
each volume at one 256 KiB chunk stored raw, so the small game makes
several volumes. The pack wrote volumes 0 to 2, the update volume 3, the
add volume 4.

- `grown.p5z`: 962,113 bytes, SHA-256
  `0b3d1fa50b0af5643d85204439c8a3a8b2de50600f8640afa67ced17f311a1d8`.
- `ampr_zstd.index`: the loose `ampr_zstd.index` stored in `grown.p5z`
  (its file record of role 2 in the `.p5z` index), taken out byte for
  byte, 2,649 bytes, SHA-256
  `dcd7ea22bae24d806978a84dec71bef2791cff2bf15699fdb925f047d765ccef`, the
  hash the engine recorded for it. It is the file the console extracts.
  (`unpack --folder` lays out the volumes of a grown `.p5z` again, so its
  `ampr_zstd.index` is another one.)
- `maxpack.image`: written from the contract (`docs/p5z-mode.md`) and the
  `volumes` of `info --play`:

      {"id":0,"offset":327463,"length":93380}
      {"id":1,"offset":420843,"length":262208}
      {"id":2,"offset":683051,"length":138060}
      {"id":3,"offset":826577,"length":93361}
      {"id":4,"offset":932609,"length":23423}

- `files.txt`: every file `ampr_zstd.index` serves, one per line:
  SHA-256, size in bytes, path. They are the records of the `.p5z`
  manifest marked packed, as the engine wrote them;
  `maxpack unpack grown.p5z --original` gives the same files, and checks
  every SHA-256.

What `ampr_zstd.index` holds (5 volumes, 54 chunks, 45 files):

| Volume | Bytes | Chunks | Bytes no chunk uses | Header counts |
|---|---|---|---|---|
| 0 | 93,380 | 39 | 92,013 | 3 volumes |
| 1 | 262,208 | 0 | 262,144 | 3 volumes |
| 2 | 138,060 | 4 | 137,856 | 3 volumes |
| 3 | 93,361 | 8 | 0 | 4 volumes |
| 4 | 23,423 | 3 | 0 | 5 volumes |

The test is `tests/p5z_grown.rs`.
