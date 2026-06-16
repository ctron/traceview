# Repository Instructions

## User-Facing Controls

When adding, removing, or changing a keyboard shortcut:

- Update the in-app help text in `src/ui.rs`.
- Update the `Controls` section in `README.md`.
- Add or update tests for the key handling behavior.

Keep the README and in-app help in sync. If a shortcut affects copied output,
filtering, selection, scrolling, or display state, include that behavior in the
tests.

## Rust Checks

Before handing off changes, run:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --tests --bins --all -- -D warnings
cargo test --locked --all-targets
```

If a change affects crates.io packaging or release metadata, also run:

```sh
cargo publish --dry-run --allow-dirty
```
