# Shrew

A local-first macOS desktop client for Jira Cloud, built for developer workflows:
status transitions, comments, and time tracking.

Every UI interaction is instantaneous; all Jira sync runs silently in the background.

## Development

Requirements: Rust 1.92+ (via [rustup](https://rustup.rs)), macOS 15+, Xcode Command Line Tools. There is no Node toolchain.

```sh
# First-time setup
xcode-select --install
cargo install sqlx-cli --features sqlite

# Run the app
cargo run -p app

# Run the tests; `-p app-core` alone does not build GPUI
cargo test --workspace
```

The workspace has two crates. `crates/core` (package `app-core`) holds the database, the sync engine and the Jira client and has no UI dependency. `crates/app` (package `app`) is the GPUI binary.

Once `crates/core/migrations` exists, the database steps apply as well:

```sh
# Create the dev DB and run migrations
export DATABASE_URL=sqlite://./crates/core/dev.db
cargo sqlx database create
cargo sqlx migrate run --source crates/core/migrations

# Bake the compile-time query cache (commit .sqlx/ to git); CI sets SQLX_OFFLINE=true
cargo sqlx prepare --workspace
```

## License

[MIT](LICENSE)
