# Security policy

## Reporting a vulnerability

Please report a security problem **privately**, not in a public issue: use GitHub's
[private vulnerability reporting](https://github.com/DeLFuS77/ampr-emu-ng/security/advisories/new)
for this repository. Reports in English or Spanish are welcome. Say which release you used (the
launch notice, for example `ampr-emu-ng: v0.5.1 (p5z) running`), the console firmware, and how to
reproduce it.

## What to know

- This project builds `libSceAmpr.sprx`, a library that runs **inside a game's process** on a
  jailbroken PS5, with that process's privileges. Only use releases from this repository's
  [Releases](https://github.com/DeLFuS77/ampr-emu-ng/releases), and check them against `SHA256SUMS`.
- The debug build (`libSceAmpr-debug.sprx`) serves metrics and failure logs over HTTP on port 9090 of
  the console, on the local network and without a password ([docs/debug-build.md](docs/debug-build.md)).
  Use it only for testing, on a network you trust.
- Everything but the p5z mode is [quer3q's upstream project](https://github.com/quer3q/ampr-emu-ng).
  A vulnerability that is also in upstream's build should be reported to upstream as well.
