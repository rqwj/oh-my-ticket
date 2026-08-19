#!/bin/bash
# repair-when-gui-stops.sh — one-shot watcher: wait until the dsh web GUI on
# port 3080 stops, then repair this conversation's session log (damaged by a
# foreign session/end-seed from a second instance sharing ~/.dsh).
# Detached with nohup; survives the death of the session that launched it.
# Log: /tmp/omt-repair-watch.log
set -u

SESSION_DIR="$HOME/.dsh/sessions/--Users-robertq-Documents-development-ts-dsh-plugins-oh-my-ticket--/session-a9fece4a-273d-41ed-a56e-492e090f6833"
REPAIR_SCRIPT="/Users/robertq/Documents/development/ts/dsh-plugins/oh-my-ticket/scripts/repair-session-log.mjs"
NODE_BIN="$(command -v node)"
DEADLINE=$(( $(date +%s) + 86400 ))  # give up after 24h

echo "[$(date '+%F %T')] watcher started (pid $$); waiting for port 3080 to free up"

while true; do
  if ! /usr/sbin/lsof -nP -ti :3080 >/dev/null 2>&1; then
    break
  fi
  if [ "$(date +%s)" -gt "$DEADLINE" ]; then
    echo "[$(date '+%F %T')] timeout after 24h — no repair performed"
    exit 1
  fi
  sleep 3
done

echo "[$(date '+%F %T')] port 3080 is free; waiting 5s for graceful flush"
sleep 5

# Guard: port may have been taken again by a restart.
if /usr/sbin/lsof -nP -ti :3080 >/dev/null 2>&1; then
  echo "[$(date '+%F %T')] port 3080 busy again (GUI restarted?) — aborting; re-run the watcher"
  exit 1
fi

echo "[$(date '+%F %T')] running repair (dry run)"
"$NODE_BIN" "$REPAIR_SCRIPT" "$SESSION_DIR" || exit 1

echo "[$(date '+%F %T')] running repair (--write)"
"$NODE_BIN" "$REPAIR_SCRIPT" "$SESSION_DIR" --write || exit 1

echo "[$(date '+%F %T')] final verification"
if "$NODE_BIN" "$REPAIR_SCRIPT" "$SESSION_DIR" | grep -q "nothing to repair"; then
  echo "[$(date '+%F %T')] SUCCESS — session log repaired; you can restart the GUI"
else
  echo "[$(date '+%F %T')] WARNING — final verification did not confirm a clean log; inspect manually"
  exit 1
fi
