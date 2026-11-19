#!/usr/bin/env python3
"""Interleaved Go-vs-Rust upload comparison against the Go fast sink. Run from tests/bench/nyuu.
usage: compare.py SIZE_MIB REPS CONNS DEPTH"""
import re, subprocess, sys, time, os, statistics as st
size, reps, conns, depth = (int(x) for x in sys.argv[1:5])
W = "work"; RS = os.environ.get("RUST_POST_BIN", "../rust-post/target/release/rust-post")
payload = f"{W}/payload-{size}.bin"
if not os.path.exists(payload):
    subprocess.run(f"head -c {size*1024*1024} /dev/urandom > {payload}", shell=True, check=True)
def run_sink(tls):
    p = subprocess.Popen([f"./{W}/nntp-sink", "-addr", "127.0.0.1:1199", "-fast"] + (["-tls"] if tls else []),
                         stdout=subprocess.PIPE, text=True)
    p.stdout.readline(); time.sleep(0.2); return p
def stop_sink(p):
    p.send_signal(2); out = p.communicate()[0]
    m = re.search(r"articles=(\d+) bytes=(\d+)", out); return int(m[1]), int(m[2])
def timed(cmd):
    t0 = time.time()
    r = subprocess.run(["/usr/bin/time", "-l"] + cmd, capture_output=True, text=True)
    wall = time.time() - t0
    m = re.search(r"([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys", r.stderr)
    rss = int(re.search(r"(\d+)\s+maximum resident set size", r.stderr)[1]) / 1048576
    return r.returncode, wall, float(m[2]) + float(m[3]), rss, r.stderr[-300:]
res = {}
for tls in (1, 0):
    cfg = f"{W}/cfg-{tls}.yaml"
    txt = open("postie.yaml").read().replace("ssl: false", f"ssl: {'true' if tls else 'false'}").replace("max_connections: 20", f"max_connections: {conns}")
    open(cfg, "w").write(txt)
    for rep in range(reps):
        for tool in ("go", "rust"):
            for f in (f"{W}/postie.db", f"{W}/postie.db-shm", f"{W}/postie.db-wal"):
                if os.path.exists(f): os.remove(f)
            os.makedirs(f"{W}/out", exist_ok=True)
            s = run_sink(tls)
            cmd = [f"./{W}/postie-cli", "-c", cfg, "-i", payload, "-o", f"{W}/out"] if tool == "go" else \
                  [RS, "--file", payload, "--conns", str(conns), "--depth", str(depth)] + (["--tls"] if tls else [])
            rc, wall, cpu, rss, err = timed(cmd)
            arts, nbytes = stop_sink(s)
            res.setdefault((tls, tool), []).append((wall, cpu, rss, arts, nbytes, rc))
            if rc: print("  rc", rc, err)
for (tls, tool), v in sorted(res.items(), reverse=True):
    wall = st.median(x[0] for x in v); cpu = st.median(x[1] for x in v); rss = st.median(x[2] for x in v)
    print(f"tls={tls} {tool:4s} wall {wall*1000:6.0f} ms  {size/wall:7.1f} MiB/s  cpu {cpu:5.2f} s ({cpu*1000/size:5.3f} ms/MiB)  rss {rss:6.0f} MiB  sink articles={v[0][3]} bytes={v[0][4]}  walls={[round(x[0]*1000) for x in v]}")
