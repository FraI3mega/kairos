#!/bin/sh
set -eu
cd "$(dirname "$0")"
rm -rf dist
for target in chrome firefox; do
  mkdir -p "dist/$target"
  cp -R src/. "dist/$target/"
  python3 tools/package.py "$target"
done
ls dist/*.zip
