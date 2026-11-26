# make_grown.py <maxpack> <fixtures dir> <work dir> <out dir>: a small real .p5z that grew, for ampr-emu-ng's
# tests, made by the MaxPack engine from the test fixtures (game/ and dlc_data/), with a README.txt.
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys

import compression.zstd as zstd

exe, fx, work, outdir = sys.argv[1:5]
LIMIT = "262208"   # one 256 KiB chunk stored raw per volume: several volumes from a small game


def run(*args):
    p = subprocess.run([exe, *args], capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit("failed: %s\n%s%s" % (" ".join(args), p.stdout, p.stderr))
    return p.stdout


shutil.rmtree(work, ignore_errors=True)
os.makedirs(work)
v1 = os.path.join(work, "v1")
shutil.copytree(os.path.join(fx, "game"), v1)
p5z = os.path.join(work, "grown.p5z")
run("pack", v1, "-o", p5z, "--p5z", "--threads", "2", "--volume-limit", LIMIT)
size_v1 = os.path.getsize(p5z)

# version 2: param.json and data/big.bin changed, data/noise.bin removed, data/new_v2.bin new
v2 = os.path.join(work, "v2")
shutil.copytree(v1, v2)
pj = os.path.join(v2, "sce_sys", "param.json")
with open(pj, encoding="utf-8") as f:
    d = json.load(f)
d["contentVersion"] = "01.001.000"
with open(pj, "w", encoding="utf-8") as f:
    json.dump(d, f)
big = os.path.join(v2, "data", "big.bin")
b = bytearray(open(big, "rb").read())
for i in range(0, len(b), 4096):
    b[i] ^= 0x5A
open(big, "wb").write(bytes(b))
os.remove(os.path.join(v2, "data", "noise.bin"))
with open(os.path.join(v2, "data", "new_v2.bin"), "wb") as f:
    f.write(b"data the second version of the game brings\n" * 7000)
run("update", p5z, v2, "--force", "--threads", "2", "--volume-limit", LIMIT)
size_v2 = os.path.getsize(p5z)
run("add", p5z, "--dlc", os.path.join(fx, "dlc_data"), "--threads", "2", "--volume-limit", LIMIT)
run("verify", p5z)
play = json.loads(run("info", "--play", p5z))

# what is in it: the index version 2, and where the chunks of ampr_zstd.index sit in each volume
raw = open(p5z, "rb").read()
foot = raw[-32:]
ioff, ilen = struct.unpack_from("<QQ", foot, 8)
ix = zstd.decompress(raw[ioff:ioff + ilen])
version, nf, nfr, nm = struct.unpack_from("<IIII", ix, 8)
prev_size, grown = struct.unpack_from("<QI", ix, 56)
slen = struct.unpack_from("<Q", ix, 24)[0]
strings = ix[len(ix) - slen:]
files = []
for k in range(nf):
    r = 72 + 88 * k
    po, pl = struct.unpack_from("<II", ix, r)
    role, storage = ix[r + 8], ix[r + 9]
    fsize, mtime, off, length, ff, nfrm = struct.unpack_from("<QqQQII", ix, r + 12)
    files.append((strings[po:po + pl].decode(), role, storage, fsize, off, length, ff, nfrm))
frames = [struct.unpack_from("<QII", ix, 72 + 88 * nf + 16 * k) for k in range(nfr)]


def stored_bytes(rec):
    _, _, storage, fsize, off, length, ff, nfrm = rec
    if storage == 0:
        return raw[off:off + length]
    return b"".join(zstd.decompress(raw[o:o + c]) for o, c, _ in frames[ff:ff + nfrm])


zrec = next(f for f in files if f[1] == 2)
zi = stored_bytes(zrec)
nvol = struct.unpack_from("<I", zi, 20)[0]
nfiles, nchunks, foff, coff, voff = struct.unpack_from("<QQQQQ", zi, 40)
chunks = [struct.unpack_from("<Q", zi, coff + 8 * k)[0] for k in range(nchunks)]
vsizes = [struct.unpack_from("<Q", zi, voff + 8 * v)[0] for v in range(nvol)]
per = {v: [] for v in range(nvol)}
for c in chunks:
    per[(c >> 32) & 0xFFF].append((c & 0xFFFFFFFF, ((c >> 44) & 0x3FFFF) + 1))
lines = []
order_broken = False
last = (-1, -1)
for c in chunks:
    key = ((c >> 32) & 0xFFF, c & 0xFFFFFFFF)
    order_broken |= key < last
    last = key
for v in range(nvol):
    used = sum(s for _, s in per[v])
    hdr = raw[play["volumes"][v]["offset"]:play["volumes"][v]["offset"] + 64]
    count = struct.unpack_from("<I", hdr, 20)[0]
    lines.append("  volume %3d: %7d bytes, %3d chunks, %7d bytes unused, header counts %d volumes"
                 % (v, vsizes[v], len(per[v]), vsizes[v] - 64 - used, count))

os.makedirs(outdir, exist_ok=True)
dst = os.path.join(outdir, "grown.p5z")
shutil.copyfile(p5z, dst)
sha = hashlib.sha256(raw).hexdigest()
readme = """grown.p5z: a small real .p5z that grew by appending (index version 2), for ampr-emu-ng's p5z mode tests.

Made by the MaxPack engine (branch pc-ampliar) from its own test fixtures (tests/tools/make_images.py and
make_fpkg.py: build/fixtures/game and build/fixtures/dlc_data), with make_grown.py:

  1. maxpack pack game -o grown.p5z --p5z --volume-limit {limit}               ({s1} bytes, version 1)
  2. maxpack update grown.p5z v2 --force --volume-limit {limit}                ({s2} bytes)
     v2 = game with sce_sys/param.json at contentVersion 01.001.000, data/big.bin changed (same size),
     data/noise.bin removed and data/new_v2.bin (308000 bytes) new
  3. maxpack add grown.p5z --dlc dlc_data --volume-limit {limit}               ({s3} bytes)
  4. maxpack verify grown.p5z: every file matches its SHA-256

Size {s3} bytes, SHA-256 {sha}.
Title PPSA00000 (a test ID), version 01.001.000, archive_id {aid}.
Index version {ver}, grown {grown} times, prev_size {prev} (the size before the last append).

Its packs (ampr_zstd.index, a loose file of the .p5z): {nvol} volumes, {nch} chunks, {order}.
The volumes from the first pack are 0..{old_last}; the update and the add appended the rest. The
headers of the volumes written before an append count the volumes there were then (fewer than the
index); the chunks of the files the update changed or removed are no longer referenced, which leaves
room in their volumes (volumes with no chunk at all among them):
{vols}

What `maxpack info --play grown.p5z` says of its volumes (maxpack.image takes these lines):
{vlines}
""".format(limit=LIMIT, s1=size_v1, s2=size_v2, s3=len(raw), sha=sha, aid=play["archive_id"], ver=version,
           grown=grown, prev=prev_size, nvol=nvol, nch=nchunks,
           order="out of record order (p5z rules only)" if order_broken else "in record order",
           old_last=max(v for v in range(nvol) if struct.unpack_from("<I", raw, play["volumes"][v]["offset"] + 20)[0]
                        == min(struct.unpack_from("<I", raw, x["offset"] + 20)[0] for x in play["volumes"])),
           vols="\n".join(lines),
           vlines="\n".join("  %d %d %d" % (x["id"], x["offset"], x["length"]) for x in play["volumes"]))
open(os.path.join(outdir, "README.txt"), "w", encoding="utf-8", newline="\n").write(readme)
print(readme)
