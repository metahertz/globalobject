#!/bin/sh
# Install or upgrade GlobalObject on Linux or macOS, then set it up.
#
#   curl -fsSL https://<control plane>/install.sh | sh -s -- --join https://<control plane>/join/<invite>
#
# Options (anything else is passed to `globalobject setup`):
#   --system         install to /usr/local/bin (uses sudo) instead of ~/.local/bin
#   --version vX.Y.Z install that release instead of the latest
#   --no-setup       only install or upgrade the binaries
#
# Environment: GLOBALOBJECT_REPO (owner/name on GitHub), GLOBALOBJECT_VERSION,
# GLOBALOBJECT_BIN_DIR, GLOBALOBJECT_DOWNLOAD_URL (a directory holding the release files,
# for mirrors and testing).
set -eu

# Release builds fill this in with the GitHub repository they were built from.
DEFAULT_REPO=""

say() { printf '%s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

repo="${GLOBALOBJECT_REPO:-$DEFAULT_REPO}"
version="${GLOBALOBJECT_VERSION:-latest}"
bin_dir="${GLOBALOBJECT_BIN_DIR:-$HOME/.local/bin}"
system=0
setup=1
pass=""
while [ $# -gt 0 ]; do
  case "$1" in
    --system) system=1; bin_dir=/usr/local/bin ;;
    --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift ;;
    --no-setup) setup=0 ;;
    *) pass="$pass $(printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")")" ;;
  esac
  shift
done

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) die "GlobalObject supports Linux and macOS so far, not $(uname -s)" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) die "no build for $(uname -m) yet" ;;
esac
target="$arch-$os"
asset="globalobject-$target.tar.gz"

if [ -n "${GLOBALOBJECT_DOWNLOAD_URL:-}" ]; then
  base="${GLOBALOBJECT_DOWNLOAD_URL%/}"
else
  [ -n "$repo" ] || die "set GLOBALOBJECT_REPO to the GitHub repository to install from (owner/name)"
  if [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
  else
    base="https://github.com/$repo/releases/download/$version"
  fi
fi

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    die "need curl or wget"
  fi
}
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else die "need sha256sum or shasum to check the download"
  fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "Downloading GlobalObject ($version, $target)..."
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"
want="$(grep " $asset\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$want" ] || die "$asset is missing from SHA256SUMS"
[ "$(sha256 "$tmp/$asset")" = "$want" ] || die "checksum mismatch for $asset: not installing it"
tar -xzf "$tmp/$asset" -C "$tmp"

if [ "$system" = 1 ] && [ "$(id -u)" != 0 ]; then sudo=sudo; else sudo=""; fi
$sudo mkdir -p "$bin_dir"
for b in globalobject globalobject-node; do
  [ -f "$tmp/$b" ] || continue
  $sudo install -m 0755 "$tmp/$b" "$bin_dir/$b"
done
say "Installed to $bin_dir: $("$bin_dir/globalobject" --version 2>/dev/null || echo globalobject)"

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) say "Note: $bin_dir isn't on your PATH. Add this to your shell profile:"
     say "  export PATH=\"$bin_dir:\$PATH\"" ;;
esac

# An existing setup is upgraded in place: restart the service so it runs the new version.
if "$bin_dir/globalobject" service status >/dev/null 2>&1; then
  "$bin_dir/globalobject" service restart || true
  say "Restarted the GlobalObject service."
  [ -z "$pass" ] && exit 0
fi

if [ "$setup" = 1 ]; then
  # Setup asks questions, and when this script is piped into sh, stdin is the script.
  if [ -r /dev/tty ]; then
    eval "set -- $pass"
    exec "$bin_dir/globalobject" setup "$@" </dev/tty
  else
    say "Run '$bin_dir/globalobject setup$pass' to finish."
  fi
fi
