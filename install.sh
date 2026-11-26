#!/bin/sh
# Installs mapterm from its GitHub releases:
#
#   curl -fsSL https://raw.githubusercontent.com/tlpinney/mapterm/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/tlpinney/mapterm/main/install.sh | sh -s -- --version v0.5.0 --dir /usr/local/bin
#
# Picks the build for this machine (macOS, Apple silicon or Intel: one universal binary,
# signed and notarized; Linux on x86-64 or ARM64), downloads it with the release's
# SHA256SUMS, and checks it: the checksum always, and the maintainer's SSH signature of
# SHA256SUMS when ssh-keygen is there, against the key pinned below (the one in
# signing/allowed_signers and on the maintainer's GitHub account). Any mismatch stops it.
# Then it puts `mapterm` in ~/.local/bin (or --dir), without sudo unless that dir needs it.
set -eu

repo="tlpinney/mapterm"
version=""
dir="${MAPTERM_INSTALL_DIR:-$HOME/.local/bin}"
# The release signing key (namespace mapterm-release, principal tlpinney).
signer='tlpinney namespaces="mapterm-release" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFSZDt6uNCxuvSttHDBUcV1dctXJ1DBwYnQdbk19FfFP'

say() { printf '%s\n' "$*" >&2; }
die() {
  say "mapterm install: $*"
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) version="${2:?--version vX.Y.Z}"; shift 2 ;;
    --dir) dir="${2:?--dir DIR}"; shift 2 ;;
    -h | --help)
      say "usage: install.sh [--version vX.Y.Z] [--dir DIR]   (default: the latest release, into ~/.local/bin)"
      exit 0
      ;;
    *) die "unknown option $1 (try --help)" ;;
  esac
done

# This machine's build (MAPTERM_INSTALL_TARGET picks another, for testing).
target="${MAPTERM_INSTALL_TARGET:-}"
[ -n "$target" ] || case "$(uname -s)" in
  Darwin) target="universal-apple-darwin" ;;
  Linux)
    case "$(uname -m)" in
      x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
      aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
      *) die "no release build for Linux on $(uname -m); build it with: cargo install mapterm" ;;
    esac
    ;;
  *) die "no release build for $(uname -s); build it with: cargo install mapterm" ;;
esac

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsL --proto '=https' --tlsv1.2 "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q --https-only "$1" -O "$2"; }
else
  die "needs curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "needs sha256sum or shasum to check the download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The latest release, unless one was asked for.
if [ -z "$version" ]; then
  fetch "https://api.github.com/repos/$repo/releases/latest" "$tmp/latest.json" || die "couldn't ask GitHub for the latest release"
  version="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmp/latest.json" | head -1)"
  [ -n "$version" ] || die "couldn't read the latest release's version"
fi
case "$version" in v*) ;; *) version="v$version" ;; esac
archive="mapterm-${version#v}-$target.tar.gz"
base="https://github.com/$repo/releases/download/$version"

say "Downloading mapterm $version for $target"
fetch "$base/$archive" "$tmp/$archive" || die "$version has no $archive (older releases had fewer builds; try: cargo install mapterm)"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "couldn't download $version's SHA256SUMS"

# The signature first: it says SHA256SUMS is the maintainer's; the checksum then says the
# archive is the one SHA256SUMS lists.
if command -v ssh-keygen >/dev/null 2>&1; then
  fetch "$base/SHA256SUMS.sig" "$tmp/SHA256SUMS.sig" || die "couldn't download $version's SHA256SUMS.sig"
  printf '%s\n' "$signer" > "$tmp/allowed_signers"
  ssh-keygen -Y verify -f "$tmp/allowed_signers" -I tlpinney -n mapterm-release -s "$tmp/SHA256SUMS.sig" < "$tmp/SHA256SUMS" >/dev/null 2>&1 \
    || die "SHA256SUMS's signature doesn't match the maintainer's key; not installing"
  say "Signature: good (tlpinney, mapterm-release)"
else
  say "Signature: not checked (no ssh-keygen here); checking the checksum only"
fi
want="$(grep "  $archive\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$want" ] || die "SHA256SUMS doesn't list $archive"
[ "$(sha256 "$tmp/$archive")" = "$want" ] || die "$archive doesn't match its checksum; not installing"
say "Checksum: good"

tar xzf "$tmp/$archive" -C "$tmp"
bin="$tmp/mapterm-${version#v}-$target/mapterm"
[ -f "$bin" ] || die "the archive has no mapterm binary"

mkdir -p "$dir" 2>/dev/null || true
if [ -w "$dir" ]; then
  install -m 755 "$bin" "$dir/mapterm"
else
  say "$dir needs sudo"
  sudo install -m 755 "$bin" "$dir/mapterm"
fi
say "Installed $("$dir/mapterm" --version 2>/dev/null || echo "mapterm $version") to $dir/mapterm"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) say "Note: $dir isn't on your PATH; add it, e.g. in ~/.zshrc or ~/.bashrc: export PATH=\"$dir:\$PATH\"" ;;
esac
