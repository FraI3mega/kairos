#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
"$here/disk-guard.sh" "$repo" sh -c 'cd "$1" && cargo build --release --locked -p kairos-server' sh "$repo"
cp "${CARGO_TARGET_DIR:-$repo/target}/release/kairos-server" "$here/kairos-server"
cd "$here"
docker compose up -d --build
docker compose ps
