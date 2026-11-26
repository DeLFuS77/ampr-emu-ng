# Changelog

What changed in each release of this copy of ampr-emu-ng. Releases are named
`v<upstream version>-maxpack.<n>`: the upstream version they are built on and a MaxPack revision.

## Unreleased

Still based on upstream **v0.5.1**.

- **Nothing recurses while a game starts.** The two sorts that still ran at module_start, on the
  game's own thread, now use the heapsort of the p5z order check, whose stack use does not grow with
  the list: the pack names of an LZ4 or pkg manifest, and the index's list of files whose path hashes
  collide. With the standard library's sort, 1,024 pack names in the worst order could take 17.6 KiB
  of that thread's stack; names already in file order never made it recurse, and no game is known to
  have hit it. Both sorts give the same result as before.

## v0.5.1-maxpack.2

Still based on upstream **v0.5.1**. Only the p5z mode changes; a game without a `maxpack.image` runs
exactly as before.

- **A `.p5z` that grew can be played.** MaxPack can now add DLC to a `.p5z`, or update it to a new
  version of the game, by appending to the end of the file. In p5z mode the chunks of each volume may
  therefore sit in any order and with room between them, a volume may hold no chunk at all, and the
  header of a volume written before the file grew may count fewer volumes than the index
  ([Growing a `.p5z`](docs/p5z-mode.md#growing-a-p5z)).
- What is still refused: two chunks that overlap, a chunk past the end of its volume, a volume header
  whose id or archive_id is not its own. The packs of other tools keep the same rules as before.
- The order check sorts a copy of the chunk table with a heapsort, so it uses no stack and takes about
  100 ms for 1.34 million chunks out of order.
- The debug build's black box says why a p5z mount was refused: `p5z mount <how> <volume>`.
- Tested with a real `.p5z` that grew, made by the MaxPack engine: every file it holds is read back and
  checked byte for byte (`crates/ampr-format/tests/p5z_grown.rs`).

## v0.5.1-maxpack.1

The first public release of MaxPack's copy of
[quer3q/ampr-emu-ng](https://github.com/quer3q/ampr-emu-ng).

- Based on upstream **v0.5.1**. Everything but the p5z mode is upstream's.
- New **p5z mode**: the zstd volumes of a game are read straight from inside one `.p5z` file, so the
  PS5 needs no `.pak` files next to the game ([docs/p5z-mode.md](docs/p5z-mode.md)). The launch notice
  reads `ampr-emu-ng: v0.5.1 (p5z) running`. A game without a `maxpack.image` runs exactly as with
  upstream.
- Release files: `libSceAmpr.sprx`, `libSceAmpr-debug.sprx` and `SHA256SUMS`.

**Legal.** MaxPack is for backups of games you own. It includes no games, firmware, keys or other
copyrighted content, does not help to obtain them and does not bypass encryption, keys or DRM;
piracy is not supported. It is provided "as is", without warranty, under the GPL-3.0-or-later: use
it at your own risk. The authors are not responsible for data loss, lost saves, damage to the
console or storage, bans or any other problem. MaxPack is not affiliated with or endorsed by Sony
Interactive Entertainment. Full text: [Legal and disclaimer](README.md#legal-and-disclaimer).
