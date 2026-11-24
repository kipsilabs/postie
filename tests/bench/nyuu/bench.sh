#!/usr/bin/env bash
# Upload benchmark: Postie's CLI against Nyuu (and optionally another postie binary, such as the
# Go-era v0.0.35 CLI) on the same in-memory NNTP sink. PAR2 and post-check are off, so every client
# does the same work: read, yEnc, POST.
#
#   [FAST=0] [POSTIE_GO_BIN=path] ./bench.sh [sizeMiB=1024] [reps=5] [tls=0|1] [conns=20]
#
# FAST=1 (default) drains POST bodies with a raw reader; FAST=0 uses the mock's textproto parser,
# which caps every client around 1 GiB/s. Runs are interleaved per repetition, and the summary is
# the median of each client. The sink's article/byte counts prove each run uploaded the whole file.
#
# Prerequisites, from this directory:
#   go build -o work/nntp-sink ./sink
#   (cd ../../../rust && cargo build --release -p postie-cli)
#   bun add nyuu@0.4.2 && bun pm trust --all     # native yencode module
set -u
cd "$(dirname "$0")"
SIZE=${1:-1024}; REPS=${2:-5}; TLS=${3:-0}; CONNS=${4:-20}; FAST=${FAST:-1}
POSTIE_BIN=${POSTIE_BIN:-../../../rust/target/release/postie}
W=work; mkdir -p $W/out
FILE=$W/payload-$SIZE.bin
[ -f "$FILE" ] || head -c $((SIZE*1024*1024)) /dev/urandom > "$FILE"
CFG=$W/postie-cfg.yaml
sed -e "s/ssl: false/ssl: $([ "$TLS" = 1 ] && echo true || echo false)/" \
    -e "s/max_connections: 20/max_connections: $CONNS/" postie.yaml > $CFG
RESULTS=$W/results.txt; : > $RESULTS

now() { python3 -c 'import time;print(time.time())'; }
run_sink() {
  ./$W/nntp-sink -addr 127.0.0.1:1199 $([ "$TLS" = 1 ] && echo -tls) $([ "$FAST" = 1 ] && echo -fast) >$W/sink.log &
  SINK=$!; sleep 0.4
}
stop_sink() {
  kill -INT $SINK 2>/dev/null; wait $SINK 2>/dev/null
  SINKSTAT=$(grep -o "articles=[0-9]* bytes=[0-9]*" $W/sink.log)
}
report() { # start end name timefile
  python3 - "$@" "$SINKSTAT" "$SIZE" "$RESULTS" <<'PY'
import sys
s, e, name, tf, sink, mb, out = sys.argv[1:8]
s, e, mb = float(s), float(e), float(mb)
t = dict(l.split() for l in open(tf) if l.strip())
cpu = float(t['user']) + float(t['sys'])
print(f'{name:9s} {(e-s)*1000:6.0f} ms  {mb/(e-s):7.1f} MiB/s  cpu {cpu:5.2f} s ({cpu*1000/mb:5.2f} ms/MiB)  sink:{sink}')
open(out, 'a').write(f'{name} {e-s} {cpu}\n')
PY
}
run_postie() { # name binary
  run_sink; rm -f $W/out/*.nzb $W/postie.db*
  local t0 t1 rc
  t0=$(now); /usr/bin/time -p -o $W/time.txt "$2" -c $CFG -i "$FILE" -o $W/out >/dev/null 2>$W/$1.err; rc=$?; t1=$(now)
  stop_sink; report "$t0" "$t1" "$1" $W/time.txt
  [ $rc -ne 0 ] && echo "  $1 rc=$rc: $(tail -2 $W/$1.err)"
}
run_nyuu() {
  run_sink; rm -f $W/out/nyuu.nzb
  local t0 t1 rc
  t0=$(now)
  /usr/bin/time -p -o $W/time.txt ./node_modules/.bin/nyuu -h 127.0.0.1 -P 1199 \
    $([ "$TLS" = 1 ] && echo "-S --ignore-cert") -n "$CONNS" -a 750000 -g alt.binaries.test \
    -f 'bench <bench@example.com>' -o $W/out/nyuu.nzb -O -q "$FILE" 2>$W/nyuu.err; rc=$?
  t1=$(now)
  stop_sink; report "$t0" "$t1" nyuu $W/time.txt
  [ $rc -ne 0 ] && echo "  nyuu rc=$rc: $(tail -2 $W/nyuu.err)"
}

echo "# ${SIZE} MiB, ${CONNS} connections, tls=${TLS}, fast-sink=${FAST}, $(uname -m) $(sysctl -n hw.ncpu 2>/dev/null || nproc) cores"
for i in $(seq 1 "$REPS"); do
  run_postie postie "$POSTIE_BIN"
  [ -n "${POSTIE_GO_BIN:-}" ] && run_postie postie-go "$POSTIE_GO_BIN"
  run_nyuu
done
echo "# medians"
python3 - "$RESULTS" "$SIZE" <<'PY'
import sys, statistics as st
rows = {}
for line in open(sys.argv[1]):
    n, wall, cpu = line.split()
    rows.setdefault(n, []).append((float(wall), float(cpu)))
mb = float(sys.argv[2])
for n, v in rows.items():
    w = st.median(x[0] for x in v); c = st.median(x[1] for x in v)
    print(f'{n:9s} {w*1000:6.0f} ms  {mb/w:7.1f} MiB/s  cpu {c*1000/mb:5.2f} ms/MiB  (n={len(v)})')
PY
