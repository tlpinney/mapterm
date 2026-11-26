#!/bin/sh
# Writes the Homebrew formula for a published release, from its signed SHA256SUMS:
#   release/brew-formula.sh v0.5.0 [path/to/homebrew-tap]
# Run it after release/sign-release.sh has published the release. It checks SHA256SUMS's
# signature against signing/allowed_signers, then writes Formula/mapterm.rb in the tap
# (default ../homebrew-tap): the signed and notarized universal binary on macOS, the
# release binaries on Linux (x86-64 and ARM64), and source for HEAD. Commit and push the
# tap yourself.
set -eu

tag="${1:?usage: release/brew-formula.sh vX.Y.Z [tap dir]}"
version="${tag#v}"
root="$(cd "$(dirname "$0")/.." && pwd)"
tap="${2:-$root/../homebrew-tap}"
repo="tlpinney/mapterm"
[ -d "$tap/Formula" ] || { echo "no Formula/ in $tap (pass the homebrew-tap checkout)" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
gh release download "$tag" --repo "$repo" --dir "$tmp" --pattern SHA256SUMS --pattern SHA256SUMS.sig
ssh-keygen -Y verify -f "$root/signing/allowed_signers" -I tlpinney -n mapterm-release \
  -s "$tmp/SHA256SUMS.sig" < "$tmp/SHA256SUMS"

sum() {
  s="$(grep "  mapterm-$version-$1.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
  [ -n "$s" ] || { echo "SHA256SUMS has no $1 archive" >&2; exit 1; }
  printf '%s' "$s"
}
mac="$(sum universal-apple-darwin)"
linux_x86="$(sum x86_64-unknown-linux-gnu)"
linux_arm="$(sum aarch64-unknown-linux-gnu)"
base="https://github.com/$repo/releases/download/$tag"

cat > "$tap/Formula/mapterm.rb" <<RUBY
class Mapterm < Formula
  desc "Map in your terminal: vector tiles, imagery, live aircraft, ships, weather"
  homepage "https://mapterm.com"
  # The release's binaries: on macOS one universal binary, signed with the maintainer's
  # Developer ID and notarized by Apple; on Linux, x86-64 and ARM64. Their SHA-256s are the
  # ones in the release's maintainer-signed SHA256SUMS.
  url "$base/mapterm-$version-universal-apple-darwin.tar.gz"
  sha256 "$mac"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  head do
    url "https://github.com/$repo.git", branch: "main"
    depends_on "rust" => :build
  end

  on_linux do
    on_arm do
      url "$base/mapterm-$version-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$linux_arm"
    end
    on_intel do
      url "$base/mapterm-$version-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$linux_x86"
    end
  end

  def install
    if build.head?
      system "cargo", "install", *std_cargo_args
    else
      bin.install "mapterm"
      doc.install "THIRD_PARTY_LICENSES.txt", "JetBrainsMono-OFL.txt"
    end
  end

  test do
    # Offline checks: the version, the help text and the built-in raster provider list.
    assert_match "mapterm $version", shell_output("#{bin}/mapterm --version")
    assert_match "esri", shell_output("#{bin}/mapterm --raster-list")
  end
end
RUBY
echo "Wrote $tap/Formula/mapterm.rb for $tag; check with: brew audit --strict --formula $tap/Formula/mapterm.rb"
