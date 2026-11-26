#!/bin/sh
# Signs and publishes a draft release made by .github/workflows/release.yml:
#   release/sign-release.sh v0.1.0-alpha.1
# Downloads the draft's archives and SHA256SUMS and checks every archive against it. The
# macOS binary is signed with the maintainer's Developer ID, notarized by Apple and
# repackaged, and its SHA256SUMS line updated. Then SHA256SUMS is signed with the
# maintainer's SSH key (namespace mapterm-release), the signature is verified against
# signing/allowed_signers and the GitHub account's keys, the changed files are uploaded,
# and the release is published. CI never holds a signing key or Apple credentials.
#
# macOS signing needs a Mac with the Developer ID certificate in the keychain and a
# notarytool keychain profile (created once with `xcrun notarytool store-credentials`; the
# one made for skycast works, as it's the same Apple account).
set -eu

tag="${1:?usage: release/sign-release.sh vX.Y.Z[-pre]}"
repo="tlpinney/mapterm"
namespace="mapterm-release"
signer="tlpinney"
key="${MAPTERM_SIGNING_KEY:-$HOME/.ssh/id_ed25519}"
identity="${MAPTERM_CODESIGN_IDENTITY:-Developer ID Application: Travis Pinney (7LATDFKMRT)}"
notary_profile="${MAPTERM_NOTARY_PROFILE:-skycast-notary}"

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp"
}
trap cleanup EXIT

draft="$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft)"
[ "$draft" = true ] || { echo "$tag is not a draft release; refusing to change it" >&2; exit 1; }

gh release download "$tag" --repo "$repo" --dir "$tmp" --pattern '*.tar.gz' --pattern SHA256SUMS
(cd "$tmp" && shasum -a 256 -c SHA256SUMS)

# macOS: sign and notarize the binary CI built (just checked against CI's SHA256SUMS), then
# repackage it and replace its checksum.
upload=""
for archive in "$tmp"/mapterm-*-apple-darwin.tar.gz; do
  [ -e "$archive" ] || continue
  [ "$(uname)" = Darwin ] || { echo "signing the macOS binary needs a Mac" >&2; exit 1; }
  name="$(basename "$archive" .tar.gz)"
  work="$tmp/macos"
  mkdir -p "$work"
  tar xzf "$archive" -C "$work"
  bin="$work/$name/mapterm"
  echo "Signing $name ($(lipo -archs "$bin")) as $identity"
  codesign --force --options runtime --timestamp --sign "$identity" "$bin"
  codesign --verify --strict --verbose=2 "$bin"

  echo "Notarizing with keychain profile $notary_profile (this can take a few minutes)"
  ditto -c -k --keepParent "$bin" "$work/notarize.zip"
  xcrun notarytool submit "$work/notarize.zip" --keychain-profile "$notary_profile" --wait \
    --output-format json > "$work/notary.json"
  status="$(sed -n 's/.*"status" *: *"\([^"]*\)".*/\1/p' "$work/notary.json")"
  if [ "$status" != Accepted ]; then
    cat "$work/notary.json" >&2
    echo "notarization $status; details: xcrun notarytool log <id> --keychain-profile $notary_profile" >&2
    exit 1
  fi
  echo "Notarized: $(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$work/notary.json")"

  rm "$archive"
  (cd "$work" && COPYFILE_DISABLE=1 tar czf "$archive" "$name")
  (cd "$tmp" && grep -v "  $name.tar.gz\$" SHA256SUMS > SHA256SUMS.new \
    && shasum -a 256 "$name.tar.gz" >> SHA256SUMS.new && mv SHA256SUMS.new SHA256SUMS)
  upload="$upload $archive"
  rm -rf "$work"
done
(cd "$tmp" && shasum -a 256 -c SHA256SUMS)

echo "Signing SHA256SUMS for $tag with $key"
ssh-keygen -Y sign -f "$key" -n "$namespace" "$tmp/SHA256SUMS"
ssh-keygen -Y verify -f "$root/signing/allowed_signers" -I "$signer" -n "$namespace" \
  -s "$tmp/SHA256SUMS.sig" < "$tmp/SHA256SUMS"
# The release notes tell people to trust the signing keys on the GitHub account, so check
# the signature against those too before publishing.
gh api "users/$signer/ssh_signing_keys" --jq ".[].key | \"$signer namespaces=\\\"$namespace\\\" \" + ." > "$tmp/github_signers"
ssh-keygen -Y verify -f "$tmp/github_signers" -I "$signer" -n "$namespace" \
  -s "$tmp/SHA256SUMS.sig" < "$tmp/SHA256SUMS"

# shellcheck disable=SC2086 # $upload is a list of paths without spaces (mktemp, archive names)
gh release upload "$tag" $upload "$tmp/SHA256SUMS" "$tmp/SHA256SUMS.sig" --repo "$repo" --clobber
gh release edit "$tag" --repo "$repo" --draft=false
echo "Published $tag: https://github.com/$repo/releases/tag/$tag"
