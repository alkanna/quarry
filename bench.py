#!/usr/bin/env python3
"""Memory and CPU benchmarks for Quarry, run from the repo root: python3 bench.py [--quick]

Uses the app's offline test hooks, so no MongoDB server is needed:
  idle    QUARRY_DEMO=1 (+ QUARRY_TEST_JSON fixture), window left alone, RSS and CPU sampled with ps.
  render  --screenshot mode, which repaints nonstop for 120 frames, with QUARRY_FRAME_LOG per-frame CPU times.

HOME points at a temp dir for every run: fixture mode without --screenshot saves open tabs
to ui.json, and that must not land in the real settings.
"""
import json, os, random, re, statistics, subprocess, sys, tempfile, time

BIN = "target/release/quarry"
QUICK = "--quick" in sys.argv
SIZES = [1_000, 10_000] if QUICK else [1_000, 10_000, 50_000]
IDLE_S = 5 if QUICK else 10
SETTLE_S = 3


def doc(i):
    r = random.Random(i)
    return {
        "_id": {"$oid": f"{i:024x}"},
        "name": f"user {i}",
        "email": f"user{i}@example.com",
        "age": r.randint(18, 90),
        "score": round(r.random() * 1000, 3),
        "active": r.random() > 0.3,
        "created": {"$date": f"2025-{r.randint(1, 12):02d}-{r.randint(1, 28):02d}T10:00:00Z"},
        "address": {"street": f"{r.randint(1, 999)} Main St", "city": r.choice(["Paris", "Lyon", "Nantes", "Lille"]), "zip": f"{r.randint(10000, 99999)}"},
        "tags": [r.choice(["a", "b", "c", "d", "e"]) for _ in range(r.randint(0, 5))],
        "orders": [{"sku": f"SKU-{r.randint(1, 500)}", "qty": r.randint(1, 9), "price": round(r.random() * 100, 2)} for _ in range(r.randint(0, 3))],
        "notes": "lorem ipsum dolor sit amet " * r.randint(0, 4),
    }


def big_doc(i):
    # About 1 MB of JSON, split across the shapes that cost differently to draw: a long string,
    # a large array of subdocuments and an object with many keys.
    d = doc(i)
    r = random.Random(i)
    d["blob"] = "".join(r.choice("abcdefghij ") for _ in range(300_000))
    d["events"] = [{"at": {"$date": "2025-06-01T10:00:00Z"}, "kind": r.choice(["view", "click", "buy"]), "value": r.random(), "path": f"/p/{r.randint(1, 9999)}"} for _ in range(5_000)]
    d["metrics"] = {f"m{k}": r.random() for k in range(4_000)}
    return d


def fixture(tmp, n, make=doc):
    path = os.path.join(tmp, f"{make.__name__}-{n}.json")
    with open(path, "w") as f:
        json.dump([make(i) for i in range(n)], f)
    return path


def footprint_mb(pid):
    # phys_footprint is what Activity Monitor shows as Memory; RSS misses compressed pages and counts shared ones.
    out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True).stdout
    m = re.search(r"Footprint:\s*([\d.]+)\s*(KB|MB|GB)", out)
    if not m:
        return None
    return float(m[1]) * {"KB": 1 / 1024, "MB": 1, "GB": 1024}[m[2]]


def idle(name, env):
    p = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    t0 = time.time()
    time.sleep(SETTLE_S)
    rss, cpu = [], []
    while time.time() - t0 < SETTLE_S + IDLE_S:
        out = subprocess.run(["ps", "-o", "rss=,%cpu=", "-p", str(p.pid)], capture_output=True, text=True).stdout.split()
        if len(out) == 2:
            rss.append(int(out[0]) / 1024)
            cpu.append(float(out[1]))
        time.sleep(0.5)
    fp = footprint_mb(p.pid)
    p.terminate()
    p.wait()
    return {"scenario": name, "rss_mb": max(rss), "footprint_mb": fp, "cpu_avg": statistics.mean(cpu), "cpu_max": max(cpu)}


def render(name, env, tmp):
    env = {**env, "QUARRY_FRAME_LOG": "1"}
    t0 = time.time()
    # /usr/bin/time -l prints peak RSS and CPU time on macOS.
    r = subprocess.run(["/usr/bin/time", "-l", BIN, "--screenshot", os.path.join(tmp, "shot.png")], env=env, capture_output=True, text=True, timeout=300)
    wall = time.time() - t0
    frames = [float(x) for x in re.findall(r"frame_ms ([\d.]+)", r.stderr)][5:]  # first frames include font atlas and fixture parse
    user = float(re.search(r"([\d.]+) user", r.stderr)[1])
    sys_ = float(re.search(r"([\d.]+) sys", r.stderr)[1])
    peak = int(re.search(r"(\d+)\s+maximum resident set size", r.stderr)[1]) / 1024 / 1024
    q = statistics.quantiles(frames, n=20) if len(frames) > 1 else [0] * 19
    return {"scenario": name, "wall_s": wall, "cpu_s": user + sys_, "peak_rss_mb": peak, "frame_p50": statistics.median(frames) if frames else 0, "frame_p95": q[18], "frame_max": max(frames, default=0)}


def table(rows):
    keys = list(rows[0])
    fmt = lambda v: f"{v:.1f}" if isinstance(v, float) else ("-" if v is None else str(v))
    print("| " + " | ".join(keys) + " |")
    print("|" + "---|" * len(keys))
    for r in rows:
        print("| " + " | ".join(fmt(r[k]) for k in keys) + " |")
    print()


def main():
    subprocess.run(["cargo", "build", "--release"], check=True)
    with tempfile.TemporaryDirectory() as tmp:
        env = {**os.environ, "HOME": tmp, "QUARRY_NO_FOCUS": "1"}
        fixtures = {f"{n:,} docs": fixture(tmp, n) for n in SIZES}
        fixtures["50 docs x 1 MB"] = fixture(tmp, 50, big_doc)
        idles = [idle("demo", {**env, "QUARRY_DEMO": "1"})]
        idles += [idle(name, {**env, "QUARRY_DEMO": "1", "QUARRY_TEST_JSON": f}) for name, f in fixtures.items()]
        renders = [render("demo", env, tmp)]
        renders += [render(name, {**env, "QUARRY_TEST_JSON": f}, tmp) for name, f in fixtures.items()]
    print(f"\n## Idle ({IDLE_S}s after {SETTLE_S}s settle; CPU in % of one core)\n")
    table(idles)
    print("## Render (120 frames, continuous repaint; frame times in ms of CPU)\n")
    table(renders)


if __name__ == "__main__":
    main()
