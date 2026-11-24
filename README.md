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

### Windows

Requirements: Windows 10 or later (x86_64), Rust 1.92+ with the `x86_64-pc-windows-msvc` toolchain, and the Build Tools for Visual Studio with the workload "Desktop development with C++", which brings the MSVC linker and the Windows SDK. The SDK is needed for more than linking: GPUI embeds its manifest with `rc.exe`, and a release build of GPUI compiles its shaders with `fxc.exe`. A build that cannot find `fxc.exe` takes its path from `GPUI_FXC_PATH`.

```sh
cargo run -p app
cargo build --release --locked -p app
```

A release build opens no console window; a dev build keeps it, as a panic is printed there. The data is in `%APPDATA%\<profile>` and the tokens are the generic credentials `jira-api-token.<profile>` and `tempo-api-token.<profile>` of the Credential Manager, where the profile is `dev.shrew.app.dev` or `dev.shrew.app` as in the table below. The cargo runner, the signing and `scripts/package.sh` are for macOS only: on Windows the release binary is `target\release\app.exe` and nothing packages it yet.

### Dev and packaged profiles

The dev build and the installed app keep their data and tokens apart, so that a sign-out, a schema experiment or a wiped database in the dev build never touches the app in daily use, and both can run at the same time:

| Build | Data (`shrew.db`, `config.json`) | Keychain service |
| --- | --- | --- |
| `cargo run -p app`, or any build not made by `scripts/package.sh` | `~/Library/Application Support/dev.shrew.app.dev` | `dev.shrew.app.dev` |
| `Shrew.app` from `scripts/package.sh` | `~/Library/Application Support/dev.shrew.app` | `dev.shrew.app` |

The tokens are the items `jira-api-token` and `tempo-api-token` of that service. The profile is fixed at compile time: `scripts/package.sh` builds with `SHREW_PACKAGED=1` (`app_core::APP_ID`). Deleting the dev data directory and its two Keychain items resets the dev build to a fresh install.

### Keychain prompt on every launch

The API tokens live in the Keychain. The Keychain ties "Always Allow" to the partition of the code that asked, and for a build the linker signed ad hoc, or one signed with a self-signed certificate, the partition is the code hash, so every rebuild asks for the login password again. For a certificate Apple issued the partition is the team, which all builds share. A free Apple Development certificate is enough; create it once:

1. Install Xcode and open Settings > Accounts.
2. Add your Apple ID, select its team and choose Manage Certificates.
3. Add an Apple Development certificate.

```sh
# Shows the new identity
security find-identity -v -p codesigning
```

The cargo runner (`scripts/dev-run.sh`, set in `.cargo/config.toml`) signs each build with this certificate before it starts. The first signing asks for access to the key and the first launch asks for each token once more: answer "Always Allow" to both. Without the certificate the builds run as before.

### UI integration tests

The tests of the `app` crate run the real views in a headless GPUI window on a fresh SQLite file, with a [wiremock](https://docs.rs/wiremock) server as the Jira site. They pass on macOS and on Windows and touch neither the Keychain, the Credential Manager nor the network.

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

## Packaging

GPUI has no bundler, so `scripts/package.sh` builds the release binary, assembles `target/package/Shrew.app` (`Info.plist` with `CFBundleName` Shrew, and `assets/icons/Shrew.icns`), signs it with the hardened runtime, wraps it in a `.dmg`, notarizes the `.dmg` and staples the ticket. Prerequisites: the Xcode Command Line Tools, a paid Apple Developer Program membership, a Developer ID Application certificate in the login keychain and a notary profile. Create the profile once; it stores an app-specific password in the Keychain:

```sh
# Lists the identity to pass as SHREW_SIGN_IDENTITY
security find-identity -v -p codesigning

# Creates the notary profile
xcrun notarytool store-credentials "<profile>" --apple-id "<apple id>" --team-id "<team id>"
```

```sh
SHREW_SIGN_IDENTITY="Developer ID Application: <name> (<team id>)" \
SHREW_NOTARY_PROFILE="<profile>" \
scripts/package.sh
```

The identity and the profile are only read from these two variables. A step whose variable is not set is skipped: without `SHREW_SIGN_IDENTITY` the bundle is not signed, without `SHREW_NOTARY_PROFILE` the `.dmg` is not notarized. The script then ends with a warning and names the file `Shrew-NOT-FOR-RELEASE.dmg`, which Gatekeeper blocks on other Macs; only a signed and notarized run writes `target/package/Shrew.dmg`. With no variables set it stops after the unsigned bundle and `.dmg`.

## License

[MIT](LICENSE)
