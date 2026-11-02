#!/bin/zsh
# Runs close_probe.sh N times, one after another, with names close-probe-1..N.
# Usage: close_probe_n.sh REEVE_BIN CWD N
here=${0:A:h}
for i in {1..$3}; do
  zsh $here/close_probe.sh $1 close-probe-$i $2
done
