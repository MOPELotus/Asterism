# Asterism

Unified learning task orchestration across multiple platforms, powered by Rust.

The project is currently building its Phase 0 core. The Rust workspace already
separates domain state, provider capabilities, events, persistence, HTTP API,
and command-line clients so provider implementations do not become coupled to
storage or user interfaces.

## Development

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Run the service and query its health endpoint:

```powershell
cargo run -p asterismd
cargo run -p asterismctl -- system health
```

The API is rooted at `/api/v1`. Until the first two provider batches are
complete, it is an internal API and may evolve without compatibility promises.
