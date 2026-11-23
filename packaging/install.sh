#!/bin/sh
# Installs Kairos for the current user: curl -fsSL https://kairos.obayy.dev/install.sh | sh
set -eu

server="${KAIROS_SERVER:-https://kairos.obayy.dev}"
server="${server%/}"

fail() {
    echo "Sorry, $1" >&2
    exit 1
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) target=linux-x86_64 ;;
    Linux-aarch64 | Linux-arm64) target=linux-aarch64 ;;
    Darwin-arm64 | Darwin-aarch64) target=macos-aarch64 ;;
    Darwin-x86_64)
        target=macos-x86_64
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
            target=macos-aarch64
        fi
        ;;
    *) fail "Kairos does not have a download for $(uname -s) on $(uname -m) yet." ;;
esac

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
else
    fail "this needs curl or wget to download Kairos, and neither is installed."
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
    fail "this needs sha256sum or shasum to check the download, and neither is installed."
fi

# Parses the pretty JSON (one key per line) kairos-release writes. Prints the version, or "<sha256> <size>".
manifest() {
    awk -v t="$2" -v f="$3" '
        { line = $0; gsub(/^[ \t]+|[ \t,]+$/, "", line) }
        line ~ /^"[^"]*": *\{$/ { k = line; sub(/^"/, "", k); sub(/": *\{$/, "", k); path[++d] = k; next }
        line ~ /^\}/ { d--; next }
        d == 0 && t == "" && line ~ /^"version":/ { v = line; sub(/^"version": *"/, "", v); sub(/"$/, "", v); print v; exit }
        d == 4 && path[1] == "targets" && path[2] == t && path[3] == "files" && path[4] == f {
            if (line ~ /^"sha256":/) { h = line; sub(/^"sha256": *"/, "", h); sub(/"$/, "", h) }
            if (line ~ /^"size":/) { s = line; sub(/^"size": */, "", s) }
        }
        END { if (h != "" && s != "") print h, s }
    ' "$1"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Looking up the latest Kairos..."
fetch "$server/download/latest/manifest.json" "$tmp/manifest.json" ||
    fail "Kairos could not reach $server. Check your internet connection and try again."
version="$(manifest "$tmp/manifest.json" "" "")"
case "$version" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "the download list from $server looks broken. Try again in a few minutes." ;;
esac

echo "Downloading Kairos $version for $target..."
for name in kairos kairosd; do
    entry="$(manifest "$tmp/manifest.json" "$target" "$name")"
    [ -n "$entry" ] || fail "Kairos $version has no $name for $target."
    want="${entry% *}"
    size="${entry#* }"
    fetch "$server/download/$version/$target/$name" "$tmp/$name" || fail "$name could not be downloaded. Try again in a few minutes."
    [ "$(wc -c <"$tmp/$name" | tr -d ' ')" = "$size" ] || fail "the downloaded $name is incomplete, so nothing was installed. Try again."
    [ "$(sha256 "$tmp/$name")" = "$want" ] || fail "the downloaded $name does not match its checksum, so nothing was installed."
    chmod +x "$tmp/$name"
done

# The script itself arrives on stdin (curl ... | sh), so install answers its questions from the terminal, or skips them.
if [ -t 1 ] && (: </dev/tty) 2>/dev/null; then
    "$tmp/kairos" install --server "$server" "$@" </dev/tty
else
    "$tmp/kairos" install --server "$server" "$@" </dev/null
fi
command -v kairos >/dev/null 2>&1 || echo "Open a new terminal to use the kairos command."
