# Cell packing checks

Run the focused Rust checks from the repository root:

```sh
cargo xtask prepare
cargo test --manifest-path tools/packing-checks/Cargo.toml --locked
```

This standalone test crate compiles the production `env_vars.rs` and
`logic/isolate.rs` modules directly. It needs no V8 build. The nine tests cover:

- S3 ETag mode parsing, quote normalization, and refusal of unsafe CAS tokens;
- the default density of 32, valid limits from 1 to 32, and invalid settings;
- growth at the configured density for a sequence of 40 cell placements;
- filling live heaps while excluding retiring heaps;
- retaining nonempty heaps and waiting for outstanding requests before freeing.

These checks do not exercise the full runtime or validate S3 durability.
