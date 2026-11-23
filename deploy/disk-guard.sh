#!/bin/sh
#   disk-guard.sh DIR COMMAND...
# Won't start COMMAND under 3 GiB free on DIR, and stops its process group under 1 GiB.
set -eu
dir=$1
shift
start_kb=${DISK_GUARD_START_KB:-3145728}
stop_kb=${DISK_GUARD_STOP_KB:-1048576}
free_kb() { df -Pk "$dir" | awk 'NR == 2 { print $4 }'; }
if [ "$(free_kb)" -lt "$start_kb" ]; then
    echo "disk-guard: only $(df -Ph "$dir" | awk 'NR == 2 { print $4 }') free on $dir, not starting: $*" >&2
    exit 1
fi
setsid "$@" &
pid=$!
trap 'kill -TERM "-$pid" 2>/dev/null || true' INT TERM
while kill -0 "$pid" 2>/dev/null; do
    if [ "$(free_kb)" -lt "$stop_kb" ]; then
        echo "disk-guard: under $((stop_kb / 1024)) MiB free on $dir, stopping: $*" >&2
        kill -TERM "-$pid" 2>/dev/null || true
        wait "$pid" || true
        exit 1
    fi
    sleep 2
done
wait "$pid"
