# Contributing

Keep Project Context application semantics in this repository. Do not move them into generic OKF repositories.

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

Changes to freshness or checkpoint semantics require tests covering clean, dirty, context-only, empty-repository, and failure-safe states.
