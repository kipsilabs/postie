#!/usr/bin/env python3
"""Interleaved par2go (via Go bench binary) vs par2-rs create, 1 GiB, Postie defaults. Run from tests/bench/nyuu."""
import os, re, shutil, subprocess, statistics as st, sys, tempfile
reps = int(sys.argv[1]) if len(sys.argv) > 1 else 5
W = "work"; RS = "../rust-par2/target/release/rust-par2"
def timed(cmd, env=None):
    r = subprocess.run(["/usr/bin/time", "-l"] + cmd, capture_output=True, text=True, env=env)
    m = re.search(r"([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys", r.stderr)
    rss = int(re.search(r"(\d+)\s+maximum resident set size", r.stderr)[1]) / 1048576
    return r.returncode, float(m[1]), float(m[2]) + float(m[3]), rss, r.stdout + r.stderr[-200:]
res = {"go": [], "rust": []}
for i in range(reps):
    env = dict(os.environ, POSTIE_BENCH_MB="1024")
    rc, wall, cpu, rss, out = timed([f"./{W}/pipeline.test", "-test.run", "^$", "-test.bench", "Pipeline/par2-only", "-test.benchtime", "1x"], env)
    ns = int(re.search(r"(\d+) ns/op", out)[1]) / 1e9
    res["go"].append((ns, cpu, rss, rc))
    d = tempfile.mkdtemp(prefix="par2rs-")
    rc, wall, cpu, rss, out = timed([RS, os.path.abspath(f"{W}/payload-1024.bin"), f"{d}/p.par2"])
    res["rust"].append((wall, cpu, rss, rc)); info = out.strip().splitlines()[0] if out.strip() else ""
    shutil.rmtree(d)
for k, v in res.items():
    w, c, r = (st.median(x[j] for x in v) for j in range(3))
    print(f"{k:4s} wall {w*1000:6.0f} ms  {1024/w:6.0f} MiB/s  cpu {c:5.2f} s  rss {r:6.0f} MiB  walls={[round(x[0]*1000) for x in v]} rc={[x[3] for x in v]}")
print("rust:", info)
