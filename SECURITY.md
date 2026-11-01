# Security

mapterm reads data from places you don't control: tile servers, styles, search results
and data files. A bug that lets that data reach your terminal as escape sequences, crash
mapterm, use unbounded memory, read local files or leak an API key is a security bug.

## Reporting

Please report security bugs privately through GitHub:
[Report a vulnerability](https://github.com/tlpinney/mapterm/security/advisories/new)
(the Security tab, then "Report a vulnerability"). Please don't open a public issue.

Include what you ran, the input that triggers it (a file, URL or style), and what
happened.

## Supported versions

Fixes go into the latest release; there are no backports before 1.0.

## Verifying releases

Release binaries come with `SHA256SUMS` and an SSH signature, `SHA256SUMS.sig`. The
signing key is published as a signing key on the maintainer's GitHub account
(`https://api.github.com/users/tlpinney/ssh_signing_keys`), fingerprint
`SHA256:LiOY0+3e/eOQFeUsG7iMovOBJui3yCAXXHlWz48c4uk`. Each release's notes show the
commands to check it.
