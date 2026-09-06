#!/bin/bash
# Compare postie's upload path with Nyuu against the same in-memory NNTP sink.
#
#   [FAST=0] ./bench.sh [sizeMiB=1024] [reps=3] [tls=0|1] [conns=20]
#   FAST=1 (default) drains POST bodies with a raw reader; FAST=0 uses the
#   mock's textproto parser, which caps both clients around 1 GiB/s.
#
# Prerequisites (run once from this directory):
#   go build -o work/nntp-sink ./sink
#   (cd ../../.. && go build -o tests/bench/nyuu/work/postie-cli ./cmd/postie)
#   bun add nyuu && bun pm trust --all      # native yencode module
#
# Reports wall time, MiB/s and client CPU (user+sys) per run; the sink's
# article/byte counts prove each run uploaded the whole file.
set -u
cd "$(dirname "$0")"
SIZE=${1:-1024}; REPS=${2:-3}; TLS=${3:-0}; CONNS=${4:-20}; FAST=${FAST:-1}
W=work; mkdir -p $W/out
FILE=$W/payload-$SIZE.bin
[ -f "$FILE" ] || head -c $((SIZE*1024*1024)) /dev/urandom > "$FILE"
CFG=$W/postie-cfg.yaml
sed -e "s/ssl: false/ssl: $([ "$TLS" = 1 ] && echo true || echo false)/" -e "s/max_connections: 20/max_connections: $CONNS/" postie.yaml > $CFG

now() { python3 -c 'import time;print(time.time())'; }
run_sink() { ./$W/nntp-sink -addr 127.0.0.1:1199 $([ "$TLS" = 1 ] && echo -tls) $([ "$FAST" = 1 ] && echo -fast) >$W/sink.log & SINK=$!; sleep 0.4; }
stop_sink() { kill -INT $SINK 2>/dev/null; wait $SINK 2>/dev/null; SINKSTAT=$(grep -o "articles=[0-9]* bytes=[0-9]*" $W/sink.log); }
report() {
  python3 - "$@" <<PY
import sys
s,e,name,sink,tf=float(sys.argv[1]),float(sys.argv[2]),sys.argv[3],sys.argv[4],sys.argv[5]
t=dict(l.split() for l in open(tf) if l.strip()); cpu=float(t['user'])+float(t['sys']); mb=$SIZE
print(f'{name:8s} {(e-s)*1000:6.0f} ms  {mb/(e-s):7.1f} MiB/s  cpu {cpu:5.2f} s ({cpu*1000/mb:5.2f} ms/MiB)  sink:{sink}')
PY
}
echo "# ${SIZE} MiB, ${CONNS} connections, tls=${TLS}, fast-sink=${FAST}, $(uname -m) $(sysctl -n hw.ncpu 2>/dev/null || nproc) cores"
for i in $(seq 1 $REPS); do
  run_sink
  t0=$(now); /usr/bin/time -p -o $W/time.txt ./$W/postie-cli -c $CFG -i "$FILE" -o $W/out >/dev/null 2>$W/postie.err; rc=$?; t1=$(now)
  stop_sink; report $t0 $t1 postie "$SINKSTAT" $W/time.txt; [ $rc -ne 0 ] && echo "  postie rc=$rc: $(tail -2 $W/postie.err)"
  run_sink; rm -f $W/out/nyuu.nzb
  t0=$(now); /usr/bin/time -p -o $W/time.txt ./node_modules/.bin/nyuu -h 127.0.0.1 -P 1199 $([ "$TLS" = 1 ] && echo "-S --ignore-cert") -n $CONNS -a 750000 -g alt.binaries.test -f 'bench <bench@example.com>' -o $W/out/nyuu.nzb -O -q "$FILE" 2>$W/nyuu.err; rc=$?; t1=$(now)
  stop_sink; report $t0 $t1 nyuu "$SINKSTAT" $W/time.txt; [ $rc -ne 0 ] && echo "  nyuu rc=$rc: $(tail -2 $W/nyuu.err)"
done
true
