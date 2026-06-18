# Repository Instructions

## User-Facing Controls

When adding, removing, or changing a keyboard shortcut:

- Update the in-app help text in `src/ui.rs`.
- There should be no controls in `README.md`; do not add a Controls section or
  document shortcuts there.
- Add or update tests for the key handling behavior.

If a shortcut affects copied output, filtering, selection, scrolling, or display
state, include that behavior in the tests.

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
