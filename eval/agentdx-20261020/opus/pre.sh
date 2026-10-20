# Session prelude: source at the start of every shell.
W=/Users/utsavsharma/.t3/worktrees/stack/t3-6c91bf15
E=$W/eval/agentdx-20261020/opus
R=$W/$(cat /tmp/agentdx-opus-results-path)
ISO=$(cat /tmp/agentdx-opus-iso)
. $E/env.sh
export RX_DIR=$R/receipts
rx() { bash $E/rx.sh "$@"; }
