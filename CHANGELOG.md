# Changelog

What changed in each release of this copy of ampr-emu-ng. Releases are named
`v<upstream version>-maxpack.<n>`: the upstream version they are built on and a MaxPack revision.

## v0.5.1-maxpack.1

The first public release of MaxPack's copy of
[quer3q/ampr-emu-ng](https://github.com/quer3q/ampr-emu-ng).

- Based on upstream **v0.5.1**. Everything but the p5z mode is upstream's.
- New **p5z mode**: the zstd volumes of a game are read straight from inside one `.p5z` file, so the
  PS5 needs no `.pak` files next to the game ([docs/p5z-mode.md](docs/p5z-mode.md)). The launch notice
  reads `ampr-emu-ng: v0.5.1 (p5z) running`. A game without a `maxpack.image` runs exactly as with
  upstream.
- Release files: `libSceAmpr.sprx`, `libSceAmpr-debug.sprx` and `SHA256SUMS`.
