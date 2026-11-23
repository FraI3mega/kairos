#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${KAIROS_BACKUPS:-/root/kairos-backups}
mkdir -p "$out"
chmod 700 "$out"
file="$out/kairos-$(date +%F).sql.gz"
docker compose -f "$here/compose.yml" exec -T db pg_dump -U kairos kairos | gzip > "$file.tmp"
# sh has no pipefail: a dump that failed still leaves a file, which must not replace a good one.
if ! gzip -dc "$file.tmp" | tail -n 8 | grep -q '^-- PostgreSQL database dump complete$'; then
    rm -f "$file.tmp"
    echo "backup: pg_dump did not finish, kept the previous dump" >&2
    exit 1
fi
mv "$file.tmp" "$file"
find "$out" -name 'kairos-*.sql.gz' -mtime +14 -delete
