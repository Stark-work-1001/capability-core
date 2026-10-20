#!/bin/bash
# Rocky-side launcher for the capability-core embedder service (port 18191).
# Uses ONLY its own venv on D:. Never touches memora venv.
DEST=/mnt/c/work/capability-core/embedder

if curl -s -m 3 http://127.0.0.1:18191/health | grep -q '"status":"ok"'; then
    echo "[INFO] already running and healthy on 18191"
    exit 0
fi

mkdir -p "$DEST/logs"
nohup "$DEST/venv/bin/python" "$DEST/embedder_service.py" > "$DEST/logs/embedder.log" 2>&1 &
echo $! > "$DEST/embedder.pid"
echo "started pid $(cat "$DEST/embedder.pid")"
echo "log: $DEST/logs/embedder.log"

i=0
while [ $i -lt 60 ]; do
    i=$((i+1))
    sleep 3
    if curl -s -m 3 http://127.0.0.1:18191/health | grep -q '"status":"ok"'; then
        echo "[OK] healthy after $((i*3))s"
        curl -s http://127.0.0.1:18191/health
        echo
        exit 0
    fi
done
echo "[ERROR] not healthy in time; tail log:"
tail -30 "$DEST/logs/embedder.log"
exit 1