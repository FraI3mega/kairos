#!/bin/sh
# Publishes a GitHub release to the download folder the server reads (KAIROS_DOWNLOADS):
#   deploy/publish-release.sh [--force] vX.Y.Z
# --force allows moving `latest` back to an older version.
set -eu

force=0
tag=
for a in "$@"; do
    case "$a" in
        --force) force=1 ;;
        -*) echo "unknown option $a" >&2; exit 2 ;;
        *) tag=$a ;;
    esac
done
[ -n "$tag" ] || { echo "usage: $0 [--force] vX.Y.Z" >&2; exit 2; }
version=${tag#v}
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' || { echo "$tag is not a release tag like v1.2.3" >&2; exit 2; }

repo_dir=$(cd "$(dirname "$0")/.." && pwd)
downloads=${KAIROS_DOWNLOADS:-/srv/kairos/downloads}
mkdir -p "$downloads"
chmod 755 "$downloads"

release_tool=${KAIROS_RELEASE:-}
if [ -z "$release_tool" ]; then
    if command -v kairos-release >/dev/null 2>&1; then
        release_tool=$(command -v kairos-release)
    else
        echo "Building kairos-release..."
        (cd "$repo_dir" && cargo build --release --locked -q -p kairos-release)
        release_tool="${CARGO_TARGET_DIR:-$repo_dir/target}/release/kairos-release"
    fi
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
tarball="kairos-dist-$version.tar.gz"
repo=${GH_REPO:-hackclub/kairos}
curl -fsSL --retry 3 -o "$tmp/$tarball" "https://github.com/$repo/releases/download/$tag/$tarball"

# Only plain folders and files, nothing that could escape the folder.
tar -tvzf "$tmp/$tarball" | while read -r mode _ _ _ _ name; do
    case "$mode" in
        d* | -*) ;;
        *) echo "$tarball contains a link or special file: $name" >&2; exit 1 ;;
    esac
    echo "$name" | grep -Eq '^(\./)?([A-Za-z0-9][A-Za-z0-9._-]*/?){0,2}$' || { echo "$tarball contains an unexpected path: $name" >&2; exit 1; }
done
mkdir "$tmp/dist"
tar -xzf "$tmp/$tarball" -C "$tmp/dist" --no-same-owner --no-same-permissions
"$release_tool" verify --dist "$tmp/dist" --version "$version"

current=$(readlink "$downloads/latest" 2>/dev/null || true)
if [ -n "$current" ] && [ "$current" != "$version" ] && [ $force = 0 ]; then
    newest=$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -n 1)
    if [ "$newest" != "$version" ]; then
        echo "latest is $current, which is newer than $version. Use --force to roll back." >&2
        exit 1
    fi
fi

if [ -d "$downloads/$version" ]; then
    if ! cmp -s "$downloads/$version/manifest.json" "$tmp/dist/manifest.json"; then
        echo "$downloads/$version already exists with different contents, refusing to overwrite it" >&2
        exit 1
    fi
    echo "$version is already unpacked"
else
    staged="$downloads/.$version.partial"
    rm -rf "$staged"
    cp -R "$tmp/dist" "$staged"
    chmod -R u=rwX,go=rX "$staged"
    mv -T "$staged" "$downloads/$version"
    echo "Unpacked $version into $downloads/$version"
fi

ln -sfn "$version" "$downloads/.latest.new"
mv -Tf "$downloads/.latest.new" "$downloads/latest"
echo "latest now points at $version"
