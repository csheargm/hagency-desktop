# Desktop development and local acceptance

[中文](local-development.zh-CN.md) · [Quick start](hagency-quickstart.md) · [README](../README.md)

## Checkout and build

The owner engine lives in `crates/hagency-client` as an independent Rust library
workspace. Desktop and owner-dashboard resolve it by local path; build and release
checks use this same tree. No sibling checkout, remote Client pin or browser UI
build is needed. See [the library guide](../crates/hagency-client/README.md).

The Desktop pins Rust 1.98.0; the server selects its own toolchain. On macOS,
install Xcode command-line tools and CMake (`brew install cmake`). Linux requires
CMake, Clang/libclang, OpenSSL, SQLite, pkg-config and native display/audio libraries;
see the dependency list in the [upstream build notes](robrix-upstream-readme.md).
Use the committed Cargo lockfile. Do not copy server target artifacts into Desktop.

```sh
cargo build --locked -p rinx --bin rinx
cargo run --locked --bin rinx
```

The package/binary is `rinx`; the application is Hagency Desktop. Default features
are sufficient for native Hagency management. On macOS Cargo uses
`packaging/run-macos.sh`, which prepares an ad-hoc signed development bundle under
`target/debug/.rinx-dev/`. Quit the previous development app before replacing that
bundle. The launcher forwards logs and shutdown to its own app process.

For debugger attachment or a stalled LaunchServices launch, run directly from the
checkout with `RINX_DEV_NO_BUNDLE=1 cargo run --locked --bin rinx`. Native permission
prompts may then be attributed to the terminal. A build or launch PID alone does
not prove the app initialized, logged in or loaded Projects.

## Isolate test accounts and data

```sh
mkdir -p .run
HAGENCY_DESKTOP_DATA_DIR="$PWD/.run/desktop-test-profile" \
cargo run --locked --bin rinx
```

Use the same absolute path for later starts. It isolates app data and cache;
`RINX_DATA_DIR` and `ROBRIX_DATA_DIR` are lower-priority aliases. Do not open two
instances against the same data directory. A new directory starts with no saved
accounts; it does not clone another account's keys, ledger or provider association.
Keep private state and logs out of Git. Logs may contain account/Room metadata.

For the server use [server setup](../../hagency-server/README.md) and
[connection diagnostics](../../hagency-server/docs/LOCAL_DEPLOYMENT.md). Start
source dev or Compose server, then enter its origin in Desktop, for example
`http://127.0.0.1:8088`. OAuth permits HTTP only for loopback IPs. A local hostname
requires HTTPS, matching Pasion issuer and trusted certificates. Check the server
container's issuer access if `auth_metadata` fails despite browser discovery working.

The saved-server dropdown supplies the origin directly. Choose **Other server…**
to enter a different address. Complete Pasion authorization in the browser; a
normal Matrix homeserver without Hagency/Pasion is not the current login target.

## Automated checks

```sh
cargo test --locked -p rinx --lib
cargo test --locked -p rinx home::project_tree::tests --lib -- --test-threads=1
cargo test --locked -p rinx browser_login_tests --lib -- --test-threads=1
cargo test --locked --manifest-path crates/article-core/Cargo.toml
python3 tools/wechat-ux/check_i18n.py
```

Project navigation tests cover separate folder expansion and Project detail
selection plus account/login refresh. Login widget tests cover saved/manual
server selection. They do not replace a native visual/pointer check. Ignored live
tests require their documented private fixtures and are not included as passed.
See [server testing](../../hagency-server/docs/TESTING.md) for protocol/database checks.

## Native acceptance sequence

1. Sign in with a disposable Pasion account. Confirm Chats synchronize and existing
   Projects appear automatically. Reopen the same profile and verify session restore.
2. Click a Project folder to expand/collapse; click its name to open details.
   Verify keyboard activation, Room selection and light/dark mode at the intended
   window size. Creating a new Project should not be needed to reveal existing ones.
3. Create/adopt an authorized Project and discussion Room, invite a test member
   and verify independent Room membership. Use another account to check owner access.
4. Create a personal Agent and wait for its owner private chat. Follow the
   [Agent quick start](hagency-quickstart.md) for Codex account, model and policy.
5. If inference is in scope, explicitly start one authorized unencrypted scope,
   send a test turn and verify an actual model response. Stop that scope when done.
   Verify stopped scopes remain stopped after restart; recovery of explicitly
   running scopes still requires fresh authority and complete local ledger.

Distinguish restored-session startup from first-ever login, fixture replies from
model replies, and an automated widget check from visible native interaction.
Unknown execution/costs must keep their holds; deleting state is not recovery.

## Packaging

Use [Octos packaging](../packaging/README-octos.md) for assistant binaries,
`packaging/build-macos-dmg.sh` for macOS distribution and
`packaging/build-ios-testflight.sh` for iOS. Distribution requires the platform's
signing configuration; the Cargo development bundle is not a release package.
[Rinx deployment](rinx-deployment.md) describes mini-app/Octos host ownership,
separately from native Hagency Agent execution.
