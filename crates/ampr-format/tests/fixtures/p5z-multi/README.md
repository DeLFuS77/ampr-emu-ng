# p5z fixtures with several volumes

The same `basic.exfat` test image as `../p5z`, packed with small volumes so
that the region of every volume but the first starts past volume 0, as in
any game over about 4 GiB of packed data. Made with the MaxPack engine CLI
(branch `consola-p5z`, commit `4f0af65`), all from ONE pack run (the
archive id changes on every pack):

    maxpack pack basic.exfat --p5z --volume-limit 524288 -o multi.p5z
    maxpack unpack multi.p5z --folder -o carpeta
    maxpack info --play multi.p5z

`--volume-limit` (for tests) caps each volume at 512 KiB, so the 2 MB of
packed data make seven volumes.

- `multi.p5z`: the packed game, seven zstd volumes stored raw inside it.
- `ampr_zstd.index`, `ampr_zstd-000.pak` to `ampr_zstd-006.pak`: copied
  from `carpeta`. Each `.pak` is byte-identical to its region of
  `multi.p5z`.
- `maxpack.image`: written from the contract (`docs/p5z-mode.md`) and the
  `volumes` of `info --play`:

      {"id":0,"offset":10149,"length":264799}
      {"id":1,"offset":274948,"length":262208}
      {"id":2,"offset":537156,"length":265802}
      {"id":3,"offset":802958,"length":262208}
      {"id":4,"offset":1065166,"length":442557}
      {"id":5,"offset":1507723,"length":286971}
      {"id":6,"offset":1794694,"length":282848}
