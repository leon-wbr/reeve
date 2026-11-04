#!/bin/zsh
# Starts one real named worker with the given reeve binary, whose only task is to write its
# report, and measures the seconds from the report's write to its Herdr pane closing, with no
# one running `reeve close`, `ls` or `watch`. Gives up after LIMIT seconds (default 600).
# Usage: close_probe.sh REEVE_BIN NAME CWD [LIMIT]
zmodload zsh/datetime
zmodload zsh/stat
bin=$1 name=$2 cwd=$3 limit=${4:-600}
state=${XDG_STATE_HOME:-$HOME/.local/state}/reeve
report=$state/reports/$name.md

$bin run --name $name --cwd $cwd -- --model haiku \
  "Write the single line 'done' to $report and stop. Do nothing else." 2>&1 | tail -1
pane=$(python3 -I -c 'import json,sys; ws=json.load(open(sys.argv[1])); ws=ws if isinstance(ws,list) else ws.get("workers",list(ws.values())); print(next(w["handle"] for w in ws if w["name"]==sys.argv[2]))' $state/workers.json $name)
start=$EPOCHREALTIME
while [[ ! -f $report ]]; do
  (( EPOCHREALTIME - start > limit )) && { echo "$name	no-report"; exit 1; }
  sleep 1
done
written=$(zstat +mtime $report)
while herdr pane get $pane >/dev/null 2>&1; do
  (( EPOCHREALTIME - written > limit )) && { echo "$name	still-open-after	$limit"; exit 1; }
  sleep 1
done
closed=$EPOCHREALTIME
printf '%s\treport_to_closed_s\t%.0f\n' $name $(( closed - written ))
