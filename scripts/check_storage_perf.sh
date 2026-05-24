#!/usr/bin/env bash
# Parses criterion's estimates and asserts the storage write throughput
# meets the M0 exit gate (>= 50k op/s).

set -euo pipefail

cd "$(dirname "$0")/.."

cargo bench -p aii-storage --bench write_throughput -- --quick \
  --output-format bencher 2>&1 | tee /tmp/aii-storage-bench.out

# bencher format (criterion 0.5): `bench: <ns> ns/iter (+/- <stddev>)`
# We have exactly one bench in this binary, so grep the single `bench:` line.
NS=$(grep -E '^bench:' /tmp/aii-storage-bench.out | head -1 | \
     sed -E 's/.*bench:[[:space:]]+([0-9,]+).*/\1/' | tr -d ',')

if [ -z "$NS" ]; then
  echo "FAIL: could not parse benchmark output"
  exit 1
fi

OPS_PER_SEC=$(( 100000 * 1000000000 / NS ))
echo "throughput = $OPS_PER_SEC ops/sec (target >= 50000)"
if [ "$OPS_PER_SEC" -lt 50000 ]; then
  echo "FAIL: throughput $OPS_PER_SEC < 50000"
  exit 1
fi
echo "OK: $OPS_PER_SEC >= 50000"
