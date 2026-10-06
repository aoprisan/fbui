# Releasing fbui to crates.io

The five workspace crates — `fbui-platform`, `fbui-testkit`, `fbui-render`,
`fbui-widgets`, `fbui` — publish together, in lockstep, at the workspace
`version`. Pushing a `v<version>` tag runs
[`.github/workflows/release.yml`](.github/workflows/release.yml), which checks,
tests, packages, uploads all five in dependency order, and creates the GitHub
release from the changelog. Versions on crates.io are permanent (yank hides a
version, it never frees the number), so the workflow refuses to publish
anything inconsistent.

## One-time setup

1. **crates.io account** — sign in at <https://crates.io> with the GitHub
   account that owns the repo; verify an email address (required to publish).
2. **First-upload token** — trusted publishing can only be configured on a
   crate that already exists, so the first release uses an API token: create
   one at <https://crates.io/settings/tokens> with the `publish-new` and
   `publish-update` scopes, then add it as the secret `CARGO_REGISTRY_TOKEN` on
   a GitHub environment named **`crates-io`** (repo Settings → Environments).
   Adding required reviewers to that environment makes every publish wait for a
   click.
3. **After the first release: switch to trusted publishing** — for each of the
   five crates, on crates.io open Settings → Trusted Publishing → Add, and enter
   owner `aoprisan`, repository `fbui`, workflow `release.yml`, environment
   `crates-io`. Then delete the `CARGO_REGISTRY_TOKEN` secret and revoke the
   token. With no secret set, the workflow exchanges its GitHub OIDC token for
   a short-lived crates.io token (`rust-lang/crates-io-auth-action`).
4. **Co-owners** (optional): `cargo owner --add <github-user> <crate>` per crate.

## Cutting a release

1. **Pick the version** per the pre-1.0 policy in `CHANGELOG.md`: bump `y` in
   `0.y.z` for breaking changes, `z` for compatible fixes/additions. An MSRV
   raise is breaking for the affected crate.
2. **Bump it in two places** in the root `Cargo.toml`:
   `[workspace.package] version`, and the `=x.y.z` pin on each of the four
   entries in `[workspace.dependencies]`.
3. **Changelog**: rename `## [Unreleased]` to `## [x.y.z] — YYYY-MM-DD — <theme>`,
   open a fresh empty `## [Unreleased]` above it, and update the link
   references at the bottom (`[Unreleased]` compares from the new tag; add an
   `[x.y.z]:` compare link).
4. **Check locally**:
   ```sh
   ./scripts/release-check.sh vX.Y.Z   # pins, changelog, tag/version agreement
   cargo test --workspace
   cargo package --workspace --exclude fbui-doc-viewer   # builds each crate from its .crate file
   ```
5. **Merge** the bump to `main` through a PR as usual, and let CI go green.
6. **Tag the merge commit and push the tag**:
   ```sh
   git tag -a vX.Y.Z -m "fbui vX.Y.Z" && git push origin vX.Y.Z
   ```
   The workflow re-runs the checks, publishes, and creates the GitHub release.

To rehearse without uploading, run the **Release** workflow by hand from the
Actions tab (`workflow_dispatch`, `dry-run` ticked — the default): everything
up to `cargo package --workspace`, no upload.

## If a publish fails partway

`cargo publish --workspace` uploads in dependency order and stops at the first
failure, so crates.io may hold some of the five at the new version. Fix the
cause on `main`, then publish only what is missing **from the tagged commit**
— never re-tag a different commit under the same version:

```sh
git checkout vX.Y.Z
cargo publish -p <crate>   # for each crate not yet on crates.io, in order:
                           # fbui-platform, fbui-testkit, fbui-render, fbui-widgets, fbui
```

If the tagged code itself is broken, yank whatever was uploaded
(`cargo yank --version X.Y.Z <crate>`) and release the next patch version.

## What gets packaged

Each crate ships its sources, tests, examples, benches, the root `README.md`
(`fbui-widgets` its own), and the two license texts (symlinked into each crate
directory; cargo copies the targets). `fbui-render` also ships the bundled Inter
font, the bitmap fonts generated from it (`fonts/*.fbf`) and their OFL
license, and the vendored tiny-skia hairline rasterizer (`src/hairline/`) with
its BSD-3-Clause license — which is why its `license` field is
`(MIT OR Apache-2.0) AND BSD-3-Clause AND OFL-1.1`. Tests and examples must
not reach outside their crate (`include_bytes!("../../…")` into a sibling):
the published package doesn't have the sibling. To prove it, extract each
`target/package/*.crate` and run its tests with the siblings patched in from
their packages. Inspect a package with
`cargo package --list -p <crate>`. docs.rs builds with the pure-Rust optional
features on (`[package.metadata.docs.rs]` in each manifest).
