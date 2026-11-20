import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

tmp = os.environ.get("RUNNER_TEMP", "/tmp")


def run(args, **kw):
    t = time.monotonic()
    out = subprocess.run(args, check=True, capture_output=True, text=True, **kw).stdout
    print(f"[time] {' '.join(args[:4])}: {time.monotonic() - t:.1f}s", flush=True)
    return out


def closure(path):
    return set(run(["nix", "path-info", "-r", path]).split())


all_paths = set(run(["nix", "path-info", "--all"]).split())
archive = json.loads(run(["nix", "flake", "archive", "--json"]))


def input_paths(node, name="self"):
    yield name, node["path"]
    for k, v in node.get("inputs", {}).items():
        yield from input_paths(v, k)


inputs = dict((p, n) for n, p in input_paths(archive))
run(["nix", "develop", "--profile", f"{tmp}/devshell", "-c", "true"])
shell = closure(f"{tmp}/devshell")
nixinst = closure("/nix/var/nix/profiles/default")
after = set(run(["nix", "path-info", "--all"]).split())
print(f"paths before develop {len(all_paths)}, after {len(after)}, new {len(after - all_paths)}")
all_paths = after


def walk(p):
    files = 0
    size = 0
    if os.path.islink(p) or not os.path.isdir(p):
        st = os.lstat(p)
        return 1, st.st_size
    for root, dirs, fs in os.walk(p):
        files += 1 + len(fs)
        for f in fs:
            try:
                size += os.lstat(os.path.join(root, f)).st_size
            except OSError:
                pass
    return files, size


def narinfo(p):
    h = os.path.basename(p).split("-", 1)[0]
    url = f"https://cache.nixos.org/{h}.narinfo"
    for attempt in range(3):
        try:
            with urllib.request.urlopen(url, timeout=20) as r:
                body = r.read().decode()
            fields = dict(line.split(": ", 1) for line in body.splitlines() if ": " in line)
            return int(fields.get("FileSize", 0)), int(fields.get("NarSize", 0))
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None
        except Exception:
            time.sleep(1)
    return "error"


t = time.monotonic()
with ThreadPoolExecutor(32) as ex:
    paths = sorted(all_paths)
    sizes = dict(zip(paths, ex.map(walk, paths)))
    infos = dict(zip(paths, ex.map(narinfo, paths)))
print(f"[time] walk + narinfo: {time.monotonic() - t:.1f}s")


def category(p):
    if p in inputs:
        return "input:" + inputs[p]
    if p in shell:
        return "shell"
    if p in nixinst:
        return "nix-install"
    return "other"


rows = {}
for p in paths:
    c = category(p)
    if c.startswith("input:"):
        c = "inputs"
    r = rows.setdefault(c, [0, 0, 0, 0, 0, 0, 0])
    f, s = sizes[p]
    r[0] += 1
    r[1] += f
    r[2] += s
    i = infos[p]
    if isinstance(i, tuple):
        r[3] += 1
        r[4] += f
        r[5] += s
        r[6] += i[0]

print()
print("category | paths | files | MB | on cache.nixos.org: paths | files | MB | download MB")
for c, r in sorted(rows.items()):
    print(f"{c} | {r[0]} | {r[1]} | {r[2] / 1e6:.0f} | {r[3]} | {r[4]} | {r[5] / 1e6:.0f} | {r[6] / 1e6:.0f}")
tot = [sum(r[i] for r in rows.values()) for i in range(7)]
print(f"total | {tot[0]} | {tot[1]} | {tot[2] / 1e6:.0f} | {tot[3]} | {tot[4]} | {tot[5] / 1e6:.0f} | {tot[6] / 1e6:.0f}")
print("narinfo errors:", sum(1 for p in paths if infos[p] == "error"))

print()
print("inputs:")
for p, n in sorted(inputs.items(), key=lambda kv: -sizes.get(kv[0], (0, 0))[0]):
    f, s = sizes.get(p, (0, 0))
    print(f"  {n} {p} files={f} MB={s / 1e6:.0f} cached={isinstance(infos.get(p), tuple)}")

print()
print("top 25 by files (not inputs):")
for p in sorted((p for p in paths if p not in inputs), key=lambda p: -sizes[p][0])[:25]:
    f, s = sizes[p]
    print(f"  {category(p)} {p} files={f} MB={s / 1e6:.0f} cached={isinstance(infos[p], tuple)}")

print()
print("not on cache.nixos.org, by MB:")
for p in sorted((p for p in paths if not isinstance(infos[p], tuple)), key=lambda p: -sizes[p][1]):
    f, s = sizes[p]
    print(f"  {category(p)} {p} files={f} MB={s / 1e6:.1f}")

nixpkgs = max(inputs, key=lambda p: sizes.get(p, (0, 0))[0])
nar = f"{tmp}/nixpkgs.nar"
with open(nar, "wb") as out:
    t = time.monotonic()
    subprocess.run(["nix", "nar", "dump-path", nixpkgs], stdout=out, check=True)
    print(f"[time] nar dump-path {nixpkgs}: {time.monotonic() - t:.1f}s, {os.path.getsize(nar) / 1e6:.0f} MB")
for i in (1, 2):
    with open(nar, "rb") as src:
        t = time.monotonic()
        subprocess.run(["nix-store", "--restore", f"{tmp}/restored{i}"], stdin=src, check=True)
        print(f"[time] nix-store --restore #{i} ({sizes[nixpkgs][0]} files): {time.monotonic() - t:.1f}s")
with open(nar, "rb") as src:
    t = time.monotonic()
    subprocess.run(["sudo", "nix-store", "--restore", "/nix/probe-restored"], stdin=src, check=True)
    print(f"[time] sudo nix-store --restore on /nix: {time.monotonic() - t:.1f}s")
tar = f"{tmp}/nixpkgs.tar"
t = time.monotonic()
subprocess.run(["gtar", "-cf", tar, "-C", nixpkgs, "."], check=True)
print(f"[time] gtar -cf: {time.monotonic() - t:.1f}s")
os.makedirs(f"{tmp}/untarred", exist_ok=True)
t = time.monotonic()
subprocess.run(["gtar", "-xf", tar, "-C", f"{tmp}/untarred"], check=True)
print(f"[time] gtar -xf ({sizes[nixpkgs][0]} files): {time.monotonic() - t:.1f}s")
sys.stdout.flush()
