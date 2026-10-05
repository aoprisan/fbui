#!/usr/bin/env bash
#
# Pre-publish consistency check for a crates.io release. Run from the repo root:
#
#   ./scripts/release-check.sh          # check the manifests and changelog
#   ./scripts/release-check.sh v0.3.0   # ...and that the tag matches them
#
# The release workflow (.github/workflows/release.yml) runs this before
# `cargo publish`; see RELEASING.md for the whole procedure. It checks that:
#   - every intra-workspace dependency pins `=<workspace version>` (lockstep),
#   - CHANGELOG.md has a `## [<version>]` section and a compare link for it,
#   - the tag, if given, is `v<workspace version>`.
set -euo pipefail

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[ -n "$version" ] || { echo "release-check: no workspace.package.version" >&2; exit 1; }
fail=0

for dep in fbui-platform fbui-render fbui-widgets fbui-testkit; do
    pinned=$(sed -n "s/^$dep = {.*version = \"\([^\"]*\)\".*/\1/p" Cargo.toml)
    if [ "$pinned" != "=$version" ]; then
        echo "release-check: [workspace.dependencies] $dep pins '$pinned', want '=$version'" >&2
        fail=1
    fi
done

if ! grep -q "^## \[$version\]" CHANGELOG.md; then
    echo "release-check: CHANGELOG.md has no '## [$version]' section" >&2
    fail=1
fi
if ! grep -q "^\[$version\]: " CHANGELOG.md; then
    echo "release-check: CHANGELOG.md has no '[$version]: <url>' link" >&2
    fail=1
fi

if [ $# -gt 0 ] && [ "$1" != "v$version" ]; then
    echo "release-check: tag '$1' does not match workspace version 'v$version'" >&2
    fail=1
fi

[ $fail -eq 0 ] && echo "release-check: v$version is consistent"
exit $fail
