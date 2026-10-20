#!/bin/bash
# Cleanly stop the capability-core embedder service (port 18191).
# SIGTERM first, verify exit, escalate to SIGKILL only if needed.
DEST=/mnt/c/work/capability-core/embedder
PIDFILE="$DEST/embedder.pid"

PIDS=""
if [ -f "$PIDFILE" ]; then PIDS=$(cat "$PIDFILE"); fi
BOUND=$(ss -tlnp 2>/dev/null | grep 18191 | grep -oP "pid=\K[0-9]+" | sort -u)
if [ -n "$BOUND" ]; then PIDS="$PIDS $BOUND"; fi

PIDS=$(echo $PIDS | tr " " "\n" | grep -E "^[0-9]+$" | sort -u | tr "\n" " ")
if [ -z "$PIDS" ]; then
    echo "[INFO] no embedder process found; nothing to stop"
    rm -f "$PIDFILE"
    exit 0
fi

echo "[INFO] stopping pid(s): $PIDS"
for p in $PIDS; do kill -TERM "$p" 2>/dev/null; done

i=0
while [ $i -lt 20 ]; do
    i=$((i+1))
    sleep 1
    STILL=""
    for p in $PIDS; do kill -0 "$p" 2>/dev/null && STILL="$STILL $p"; done
    if [ -z "$STILL" ]; then
        echo "[OK] stopped cleanly after ${i}s"
        rm -f "$PIDFILE"
        exit 0
    fi
done

echo "[WARN] still alive:$STILL -- sending SIGKILL"
for p in $PIDS; do kill -KILL "$p" 2>/dev/null; done
sleep 2
rm -f "$PIDFILE"
echo "[OK] force-stopped"