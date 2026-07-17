# Repository Guidelines

## Project Structure & Module Organization

This Rust workspace has five framework crates. `fbui-platform/` owns Linux display, input, seat, VT, and event-loop backends. `fbui-render/` provides the headless CPU renderer, while `fbui-widgets/` contains the retained widget tree and controls. `fbui/` is the umbrella crate and runner; `fbui-testkit/` supplies golden-PNG assertions. Source lives under each crate's `src/`; integration and snapshot tests live in `tests/`, with goldens in `tests/snapshots/`. Examples are under `examples/`, documentation is in `docs/`, and device helpers are in `scripts/`. `spikes/` is a separate historical crate; do not add framework features there.

Respect the dependency layering documented in `CLAUDE.md` and consult `PLAN.md` plus the relevant `PHASE*.md` before architectural changes. Platform types must not leak into headless widgets.

## Build, Test, and Development Commands

- `cargo test --workspace` runs headless unit, behavior, and snapshot tests.
- `cargo fmt --all -- --check` verifies formatting; run `cargo fmt --all` to apply it.
- `cargo clippy --workspace --all-targets` runs the CI lint gate.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` checks documentation.
- `cargo bench -p fbui-widgets --bench scroll` exercises the scroll performance gate.
- `cargo run -p fbui --example showcase --features platform` runs the showcase on Linux using a real text VT or terminal fallback.
- `cd spikes && cargo build --release` builds the excluded spike separately.

## Coding Style & Naming Conventions

Use Rust 2021 and standard `rustfmt` output (four-space indentation). Follow Rust naming: `snake_case` modules/functions/tests, `CamelCase` types/traits, and `SCREAMING_SNAKE_CASE` constants. Keep public APIs documented and Clippy-clean. Preserve renderer/platform invariants: use kernel-reported stride, write frame buffers sequentially, restore the VT on every exit path, and keep animation deterministic and damage-driven.

## Testing Guidelines

Place unit tests near their modules and cross-module tests in `<crate>/tests/`; name tests after observable behavior. Rendering changes require snapshot coverage. Regenerate intentional goldens with `FBUI_UPDATE_SNAPSHOTS=1 cargo test -p <crate> --test <snapshot-test>`, then inspect and commit the PNGs. Device tests in `fbui-platform/tests/integration.rs` are ignored by default and require VKMS/uinput plus privileges; see `scripts/qemu-test.sh`.

## Commit & Pull Request Guidelines

History follows Conventional Commit-style subjects: `feat(widgets): ...`, `fix: ...`, `docs: ...`, and `refactor(widgets): ...`. Keep commits focused and use an imperative, specific subject. Pull requests should explain motivation and behavior, identify affected crates/features, link relevant issues, and report tests run. Include before/after screenshots or updated golden PNGs for visual changes, and call out hardware-only validation or remaining gaps.
