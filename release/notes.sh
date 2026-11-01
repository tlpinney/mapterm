#!/bin/sh
# Prints the GitHub release notes for a tag: the version's CHANGELOG.md section, then how to
# install and verify the binaries.
#   release/notes.sh v0.1.0-alpha.1
set -eu

tag="${1:?usage: release/notes.sh vX.Y.Z[-pre]}"
version="${tag#v}"
root="$(cd "$(dirname "$0")/.." && pwd)"

# The section between "## [version]" and the next "## [" heading, without the heading.
awk -v v="$version" '
  index($0, "## [" v "]") == 1 { on = 1; next }
  on && /^## \[/ { exit }
  on && !/^\[[^]]*\]: / { print }
' "$root/CHANGELOG.md" | sed -e '/./,$!d'

cat <<NOTES

### Binaries

| platform | file |
|---|---|
| macOS, Apple silicon | \`mapterm-$version-aarch64-apple-darwin.tar.gz\` |
| Linux, x86-64 (glibc) | \`mapterm-$version-x86_64-unknown-linux-gnu.tar.gz\` |

\`SHA256SUMS\` lists every archive's checksum, and \`SHA256SUMS.sig\` is the maintainer's SSH
signature of it. The trusted key comes from the signing keys on the maintainer's GitHub
account, not from this repository (fingerprint
\`SHA256:LiOY0+3e/eOQFeUsG7iMovOBJui3yCAXXHlWz48c4uk\`). To download and check (the
repository is private, so use \`gh\`):

\`\`\`sh
gh release download $tag --repo tlpinney/mapterm
gh api users/tlpinney/ssh_signing_keys --jq '.[].key | "tlpinney namespaces=\\"mapterm-release\\" " + .' > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I tlpinney -n mapterm-release -s SHA256SUMS.sig < SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
tar xzf mapterm-$version-*.tar.gz
\`\`\`

Or build it with Cargo (\`CARGO_NET_GIT_FETCH_WITH_CLI\` lets Cargo use git's credentials for
the private repository; Rust 1.89 or newer):

\`\`\`sh
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --git https://github.com/tlpinney/mapterm --tag $tag
\`\`\`

The macOS binary isn't notarized: files downloaded with a browser need
\`xattr -d com.apple.quarantine mapterm\` once (\`gh\` and \`curl\` downloads don't).
NOTES
