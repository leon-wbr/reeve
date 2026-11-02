#!/bin/bash
# The day scenario at several handoff thresholds: `thresholds.sh N...` runs trial N of each cell as
# rd-<ARM>-h<K>k-<N>, so day-score keeps each threshold apart. Run it detached with nohup.
# REEVE_BENCH_DIR is the trusted trials dir; BIN holds the reeve and reeve-bench to test.
# Every run goes through `reeve heavy --lock reeve-bench`, after a gap that lets other bench
# batches take the lock. Runs that have a result are skipped, so a stopped batch resumes.
set -u
: "${REEVE_BENCH_DIR:?set REEVE_BENCH_DIR to the trials dir}" "${BIN:?set BIN to the binaries dir}"
export REEVE_BENCH_DIR
CELLS="${CELLS:-reeve:35 reeve:45 reeve:60 reeve:350 b0:45}"  # arm:budget in K
for n in "$@"; do
  for cell in $CELLS; do
    arm=${cell%%:*}; k=${cell##*:}
    run="h${k}k-$n"
    if [ -f "$REEVE_BENCH_DIR/logs/rd-$arm-$run/result.json" ]; then echo "== skip $arm $run (has a result)"; continue; fi
    sleep "${GAP:-120}"
    echo "== $(date -u +%FT%TZ) start $arm $run budget ${k}000"
    "$BIN/reeve" heavy --slots "${SLOTS:-4}" --lock reeve-bench --label "bench-day-$arm-$run" --timeout 80m -- \
      timeout 4500 "$BIN/reeve-bench" day "$arm" "$run" --budget "${k}000"
    echo "== $(date -u +%FT%TZ) end $arm $run exit $?"
  done
done
echo "== batch done"
