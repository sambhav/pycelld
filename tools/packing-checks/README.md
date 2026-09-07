# Cell packing checks

Run the focused Rust checks from the repository root:

```sh
cargo xtask prepare
cargo test --manifest-path tools/packing-checks/Cargo.toml --locked
```

This standalone test crate compiles the production `logic/isolate.rs` module
directly. It needs no crate dependencies or V8 build. The three tests cover:

- growth at the configured density for a sequence of 40 cell placements;
- filling live heaps while excluding retiring heaps;
- retaining nonempty heaps and waiting for outstanding requests before freeing.

These checks do not exercise the full runtime or validate S3 durability.

Environment and S3 ETag parsing are tested through the actual celld library by `cargo xtask test`,
including upstream node identity validation.
