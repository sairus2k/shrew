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

### Keychain prompt on every launch

The API token lives in the Keychain. The linker signs a build ad hoc, and the Keychain ties "Always Allow" to the code hash of an ad hoc binary, so every rebuild asks for the login password again. To stop it, create a local code signing certificate once:

```sh
scripts/create-dev-cert.sh
```

The cargo runner (`scripts/dev-run.sh`, set in `.cargo/config.toml`) signs each build with this certificate before it starts. The certificate lives in a keychain of its own, `~/Library/Keychains/shrew-dev.keychain-db`, so signing brings up no dialog. The first launch asks for the token once more: answer "Always Allow". Without the certificate the builds run as before.

### UI integration tests

The tests of the `app` crate run the real views in a headless GPUI window on a fresh SQLite file, with a [wiremock](https://docs.rs/wiremock) server as the Jira site. They need macOS, as the app itself does, so CI needs a macOS runner. They touch neither the Keychain nor the network.

```sh
# All of them; they are part of `cargo test --workspace` as well
cargo test -p app

# One module or one test
cargo test -p app harness::
```

The shared helpers are in `crates/app/src/harness.rs`. `launch_on_jira` starts the app against a mock Jira that serves the given issues, and `wait_until` waits for a visible condition: the core works on tokio threads, so `cx.run_until_parked()` does not wait for it.

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
