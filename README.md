# ampr-emu-ng

A hard fork of [drakmor/ampr_emu](https://github.com/drakmor/ampr_emu), built to run PKG, zstd, LZ4 and plain games quickly and reliably.

## This copy

This repository is MaxPack's copy of
[quer3q/ampr-emu-ng](https://github.com/quer3q/ampr-emu-ng), the original
project by quer3q. It follows upstream and adds one mode, **p5z**: the zstd
volumes of a game are read straight from inside one `.p5z` file, so the
PS5 needs no `.pak` files next to the game
([docs/p5z-mode.md](docs/p5z-mode.md)). A game without a `maxpack.image`
runs exactly as with upstream.

MaxPack is made of three repositories:

- [DeLFuS77/maxpack](https://github.com/DeLFuS77/maxpack) is the PC app and
  command line that pack a game into a `.p5z`, and has the
  [user guide](https://github.com/DeLFuS77/maxpack/blob/main/docs/guide.md).
- [DeLFuS77/maxpack-ps5](https://github.com/DeLFuS77/maxpack-ps5) is the PS5
  payload that mounts a `.p5z` game, writes its `maxpack.image` and ships this
  `libSceAmpr.sprx`.
- [DeLFuS77/ampr-emu-ng](https://github.com/DeLFuS77/ampr-emu-ng) is this
  repository.

Releases of this copy are named `v<upstream version>-maxpack.<n>`, for
example `v0.5.1-maxpack.1`: the upstream version it is built on and a
MaxPack revision. The launch notice shows the upstream part only
(`ampr-emu-ng: v0.5.1 (p5z) running`). Download them from
[Releases](https://github.com/DeLFuS77/ampr-emu-ng/releases), with
`SHA256SUMS`. Report a problem with a `.p5z` game here; a problem that also
happens with upstream's build belongs to upstream.

## How to

### Use

Copy `libSceAmpr.sprx` into the game's `fakelib/`, mount the game with
ShadowMountPlus and turn off fakelib updates in its settings. At launch the
console shows `ampr-emu-ng: vx.x.x (zstd) running`, where the mode is `zstd`,
`p5z` (zstd volumes read from inside one `.p5z` file,
[docs/p5z-mode.md](docs/p5z-mode.md)), `pkg`, `lz4`, or `plain` for a game
with only loose files.

A PKG game runs through [kraken-mount](https://github.com/quer3q/kraken-mount),
the PKG launcher, which copies `libSceAmpr.sprx` into the game's `fakelib/`
itself. A `.p5z` game runs through
[maxpack-ps5](https://github.com/DeLFuS77/maxpack-ps5), which does the same
with its own copy of this build.

### Make packs

[ps5-dump-forge](https://github.com/quer3q/ps5-dump-forge) builds PKGs, zstd
packs and LZ4 packs. [MaxPack](https://github.com/DeLFuS77/maxpack) builds
zstd packs and the `.p5z` that holds them. Other tools that write the same
formats should work too, but are untested.

### Build

The build needs Docker. The first run builds the toolchain image
`ampr-emu-ng-ps5:latest` from `ps5/Dockerfile` (Rust 1.100.0, LLVM 19,
ps5-payload-sdk v0.45); `AMPR_PS5_IMAGE` picks another image.

```sh
./ps5/build-docker.sh            # make dist
```

`dist` builds the PRX, checks it with `tools/verify_payload_prx.py` and
copies two files to the project root:

- `libSceAmpr.sprx`, the release build;
- `libSceAmpr-debug.sprx`, the same runtime plus metrics, tuning and
  failure logs, for testing ([docs/debug-build.md](docs/debug-build.md)).

Every push to main also publishes both files as the
[beta pre-release](https://github.com/DeLFuS77/ampr-emu-ng/releases/tag/beta);
its launch notice reads `v<version>-beta.<commit>`.

Design notes, the memory budget, the PRX size rule and how a change is
checked are in [DESIGN.md](DESIGN.md).

## Legal note

This project ships no games and nothing copyrighted, and it touches no keys,
encryption or DRM. It is meant for backups of games you own, dumped from your
own console. It is not affiliated with or endorsed by Sony.

## Help and reports

Game reports, bug reports and ideas are welcome, in English or Spanish: use the
[issue templates](https://github.com/DeLFuS77/ampr-emu-ng/issues/new/choose).
For a security problem, see [SECURITY.md](SECURITY.md). What changed in each
release is in [CHANGELOG.md](CHANGELOG.md).

## Thanks

- [quer3q/ampr-emu-ng](https://github.com/quer3q/ampr-emu-ng), the
  project this copy follows; everything but the p5z mode is upstream's.
- [drakmor/ampr_emu](https://github.com/drakmor/ampr_emu), the C++ AMPR
  emulator this project ports.
- [ps5-payload-sdk](https://github.com/ps5-payload-dev/sdk), the PS5
  toolchain and libraries.
- [AnyPS5](https://github.com/boykopovar/AnyPS5), for its libSceAmpr
  implementation and notes on the libc exports of retail libSceAmpr.

## License

GPL-3.0-or-later, the same as upstream (`LICENSE`).
