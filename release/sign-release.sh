#!/bin/sh
# Signs and publishes a draft release made by .github/workflows/release.yml:
#   release/sign-release.sh v0.1.0-alpha.1
# Downloads the draft's archives and SHA256SUMS, checks every archive against it, signs it
# with the maintainer's SSH key (namespace mapterm-release), verifies the signature against
# signing/allowed_signers, uploads SHA256SUMS.sig, and publishes the release. CI never
# holds the signing key.
set -eu

tag="${1:?usage: release/sign-release.sh vX.Y.Z[-pre]}"
repo="tlpinney/mapterm"
namespace="mapterm-release"
signer="tlpinney"
key="${MAPTERM_SIGNING_KEY:-$HOME/.ssh/id_ed25519}"

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
cleanup() {
  rm -f "$tmp"/*
  rmdir "$tmp"
}
trap cleanup EXIT

draft="$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft)"
[ "$draft" = true ] || { echo "$tag is not a draft release; refusing to change it" >&2; exit 1; }

gh release download "$tag" --repo "$repo" --dir "$tmp" --pattern '*.tar.gz' --pattern SHA256SUMS
(cd "$tmp" && shasum -a 256 -c SHA256SUMS)

echo "Signing SHA256SUMS for $tag with $key"
ssh-keygen -Y sign -f "$key" -n "$namespace" "$tmp/SHA256SUMS"
ssh-keygen -Y verify -f "$root/signing/allowed_signers" -I "$signer" -n "$namespace" \
  -s "$tmp/SHA256SUMS.sig" < "$tmp/SHA256SUMS"

gh release upload "$tag" "$tmp/SHA256SUMS.sig" --repo "$repo" --clobber
gh release edit "$tag" --repo "$repo" --draft=false
echo "Published $tag: https://github.com/$repo/releases/tag/$tag"
