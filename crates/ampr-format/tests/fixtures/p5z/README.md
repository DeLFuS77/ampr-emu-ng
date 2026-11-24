# p5z fixtures

Made with the MaxPack engine CLI (branch `consola-p5z`, commit `5780807`,
tag `motor-consola-2`) from its `basic.exfat` test image, all from ONE pack
run (the archive id changes on every pack):

    maxpack pack basic.exfat --p5z -o basic.p5z
    maxpack unpack basic.p5z --folder -o carpeta
    maxpack info --play basic.p5z

- `basic.p5z`: the packed game, one zstd volume stored raw inside it.
- `ampr_zstd.index`, `ampr_zstd-000.pak`: copied from `carpeta`. The
  `.pak` is byte-identical to the region of `basic.p5z`.
- `maxpack.image`: written by hand from the contract (`docs/p5z-mode.md`)
  and the volume line of `info --play`:
  `{"id":0,"offset":10149,"length":2067009}`.
