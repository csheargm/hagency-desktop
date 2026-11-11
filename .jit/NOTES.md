# Hagency Desktop on the makepad JIT line: port notes

The desktop app (package `rinx`) builds against the makepad JIT line: upstream makepad `work`
(the HAL rewrite with the relax JIT) plus the fixes on OctoSense-org/makepad branch `hagency-jit`.
It runs two ways from the same sources:

- under the JIT runner, `makepad run`, where an edit is patched into the running app in about
  0.1 s instead of a 10–37 s cargo rebuild and restart;
- as a plain `cargo build`, which is what CI and release packaging use.

STATUS 10 Nov 2026: runs signed in, `html_preview` included. The JIT run is checked on an Apple
silicon Mac, where its log lists one body it cannot compile (in typewit, never reached), and it
starts on Linux x86_64; the cargo build has also been run on Linux x86_64 and arm64, Intel macOS
and Windows (see "Platforms the build was run on").

## Setup

    .jit/sources.sh            # fetches the pinned makepad checkout into .sources/ (git-ignored)

`.jit/sources.lock` pins one checkout, `.sources/makepad` (OctoSense-org/makepad `hagency-jit`),
and `Cargo.toml` reaches it by path; every URL the dependency graph names makepad by resolves
to it. `compat/` there keeps the fork's `makepad-markdown` and `makepad-markdown-widgets` crate
names. The Octoscript-Makepad kits are an ordinary git dependency, pinned on that repository's
`jit` branch. The CI workflows run the same script after checking out this repository.

A checkout that already exists is left alone, so `.sources/makepad` may be a symlink to a working
tree of your own; `.jit/sources.sh --check` says whether it is at the pin. Without the checkout
no cargo command works here, because the manifest names it.

The standalone workspaces that are built and tested on their own use the same checkout through
`../../.sources/makepad`: `crates/article-makepad`, `crates/owner-dashboard` and
`tools/hagency-script-review` (so mini-app scripts are reviewed on the script VM the app runs).
`tools/windows-render-probe` still pins the fork makepad; it is a Windows release tool and was
not ported.

The runner is built from that same makepad tree. It builds makepad's own proc macros into
itself and refuses a program whose makepad tree differs, so rebuild it whenever the pin moves:

    cd .sources/makepad
    cargo build --release -p makepad-run -p relax-runner     # target/release/makepad and relax

## Run

    export MAKEPAD_RUN_ALLOW_C=1 MAKEPAD_RUN_TRUST_PROC_MACROS=1
    .sources/makepad/target/release/makepad run --allow-c rinx

- `MAKEPAD_RUN_ALLOW_C=1` / `--allow-c`: dependencies with C or Swift build steps link natively.
- `MAKEPAD_RUN_TRUST_PROC_MACROS=1`: third-party proc macros run unsandboxed, as cargo runs them.
- A development run should not share state with an installed Hagency Desktop. Give it its own
  `HAGENCY_DESKTOP_DATA_DIR` and `HAGENCY_OWNER_DAEMON_HOME`, and do not use hosting Setup from
  it: Setup replaces the owner-daemon LaunchAgent, which on a machine with the installed app is
  that app's service.
- `RINX_OCTOS_BIN` must name an octos binary for `packaged_kernel()` in a development run.
- The first run compiles everything (about 1.5 min on an M-series MacBook); later runs start
  from the cache in `target/makepad-run`. That cache grows to tens of GB; `makepad clean`
  reclaims it.

Headless: add `MAKEPAD_HIDE_WINDOWS=1 MAKEPAD_NO_FOCUS=1`, read the pid from the
`makepad-remote: pid <n>` line the runner prints, then drive the app with
`makepad inspect <pid> ui`, `ui act node=<id> press`, `ui type node=<id> t=<text>`,
`ui wait q=<text>`, `grab out=<file.png>` and `quit`.

## Tests

    relax=.sources/makepad/target/release/relax
    $relax check --allow-c rinx                                # type and borrow check
    $relax selftest rinx --allow-c                             # compiles every reachable fn
    $relax test rinx --lib --allow-c -- --test-threads=1       # the app's unit tests under the JIT

Results on 10 Nov 2026 (Apple silicon Mac):

- JIT: selftest 26 crates, 1,233,542 fns, 0 failed builds, 0 errors, no unresolved imports;
  app suite 800 passed, 0 failed, 2 ignored; with `--features agent_chat,octosense-module`
  802 passed, 0 failed, 2 ignored.
- cargo: `cargo test --locked --features agent_chat,octosense-module --lib` 802 passed,
  0 failed, 2 ignored; `cargo build --locked --features agent_chat` links, and that binary
  runs signed in like the JIT run.
- `python3 tools/check-hagency-regressions.py` (the release gate): 117 checks passed, exit 0.
- With `-D warnings` off, the five desktop CI builds (Ubuntu x86_64 and arm64, macOS arm64 and
  x86_64, Windows) compile and link this line.
- `cargo check --locked --no-default-features --features agent_chat` fails here exactly as it
  does on the fork line (`OwnerGrant*` imports); it is not a port problem.
- With `html_preview`: `cargo test --locked --features agent_chat,octosense-module,html_preview
  --lib` 808 passed, 0 failed, 2 ignored; `$relax test rinx --lib --allow-c --features
  agent_chat,html_preview -- --test-threads=4` 806 passed, 0 failed, 2 ignored (peak resident
  memory of the JIT run: about 20 GB).

With the pin of OctoSense-org/makepad#140 (the compiler and std fixes listed under "Fixed in
makepad after the port") on `main` at c0242243 (11 Nov 2026), Apple silicon Mac, `html_preview`
on:

- `$relax test rinx --lib --allow-c --features agent_chat,html_preview -- --test-threads=1`:
  859 passed, 0 failed, 2 ignored.
- `makepad run` with a hidden window: the sign-in screen. The log lists no compile error with
  an error code (371 lines on 10 Nov before makepad#138) and one line without a code, for
  typewit (see "Gaps against the fork build").
- `$relax selftest rinx --allow-c --features html_preview`: 26 crates, 1,347,688 fns, 0 failed
  builds, 0 errors (peak resident memory about 20 GB).

Memory of the test run. A dozen tests (in `matrix_links`, `matrix_context`,
`verification_modal`, `settings::crypto_settings` and `profile::user_verification_cache`)
run themselves again in a child process through `current_exe()`. Under the JIT the child is a
whole JIT process: about 5 GB with a warm cache, more while it compiles. The peak of the suite
therefore follows `--test-threads`:

| `--test-threads` | cold cache | warm cache |
|---|---|---|
| 1 | 20 to 24 GB (Mac) | 9 GB (Mac), 11 GB (Linux) |
| 4 | 42 to 47 GB (Linux) | 24 to 26 GB (Linux) |

On Linux the results are the same either way. The cache is cold again whenever the makepad pin or the JIT's
std changes. A cold run also takes three `article_app::preview` tests past their 10 s limit
(one times out, two then find its lock poisoned); they pass on the next run.

A JIT compiler fault can take tens of GB before it stops: a single-file test with
`#[derive(Debug)]` on a 300-variant enum reached 69 GB before makepad#138 fixed that one (it now
takes 0.2 s). On a shared machine run `relax` and `makepad run` experiments under a memory limit.

## Signed-in session test

The app signs in only to Hagency servers through browser OAuth. For a headless run:

- build with `--features palpo-instrument` and set `HAGENCY_INSTRUMENT_LOGIN_URL=<file>`: the
  app writes the authorization URL to that file instead of opening a browser;
- the test server is a local Matrix homeserver behind a small loopback front that serves the
  Hagency discovery document and grants the OAuth authorization to one test account. A script
  fetches the URL from the file to complete the redirect.

Checked this way, cold cache and warm: sign-in through the UI, the room list, a timeline with
state events, plain and HTML messages, a mermaid diagram and a link with its preview card, a
markdown message sent from the composer, quit, and session restore on the next start.

## HTML/CSS preview (`html_preview`)

The feature is on again. `makepad-html-renderer` is OctoSense-org/makepad-html at branch
`hagency-jit`: that repository's main plus one manifest line (makepad-widgets without the
`serde` feature, which this line's makepad-widgets does not have). The renderer's code is what
the app pinned before the port. As before the port it is not a default feature:

    cargo build --features html_preview
    .sources/makepad/target/release/makepad run --allow-c rinx --features html_preview

It is the Article editor's "HTML/CSS preview (experimental)" page, and it opens only for an
article whose source is HTML (one imported from an `.html` file). From a room: composer plus
menu, Article editor, the article, Full preview, then the preview button. A published one is
under Published, Publication record, read full article.

Checked on 10 Nov 2026 with the cargo build and with the JIT run, both headless: the page
reports "HTML/CSS preview ready", and the two builds draw the same bitmap for the same article
(headings, bold text, a list, a bordered table, a link; 268,400 pixels compared, none differ).
The app's sanitizer removes `<style>` blocks before the renderer sees the document.

## Platforms the build was run on

10 Nov 2026, the cargo build, driven headless through the makepad remote instrument:

- macOS arm64: everything in this file.
- Linux x86_64 (Ubuntu, headless Wayland): the signed-in session test above, end to end. A
  bare Ubuntu host needs `libfontconfig1-dev` to build.
- Linux arm64, macOS x86_64 and Windows x86_64 (GitHub runners): the app starts, draws the
  sign-in screen, takes text input and a button press, reaches the single sign-on screen through
  HTTP discovery against a loopback stub, and quits with exit code 0. It was not signed in there.
- Windows needed a makepad fix, which the pin has: on a machine whose only Direct3D adapter is
  the software one (a VM, a CI runner) the device was refused and the app exited at startup.
  `release-verify.yml` smoke-tests the Windows installer on such a runner.

The JIT runner (`makepad run`) is checked on macOS arm64. On Linux x86_64 (Ubuntu, headless
Wayland) it now works as far as it was tried: `relax check --allow-c rinx --features
agent_chat,html_preview` reports no error, the app's unit tests under the JIT are 856 passed,
1 failed, 2 ignored (`main` at c0242243), and the app starts and draws the sign-in screen. The
test that fails,
`shared::web_browser::session_tests::browser_failure_is_scoped_to_its_tab_and_a_new_navigation_clears_it`,
expects the status text the app shows on other systems (a `cfg` in `src/shared/web_browser.rs`);
`cargo test` was not run on Linux to compare. The app was not signed in there, the preview was
not opened, and x86 SIMD is incomplete (see "Gaps against the fork build"). The JIT runner was
not tried on Windows.

## Fixed in makepad after the port

In the pin since OctoSense-org/makepad#137:

- RoomScreen is a view that also names a widget (`#[deref]` plus `#[find]`), and the widget
  derive listed only the named widget's children for such a type. About twenty lookups from
  `self` came back empty in the cargo build and in the JIT run alike; the composer's plus menu
  never opened. No app change was needed.
- The Direct3D device on a software adapter (above).
- What `html_preview` needed from the JIT compiler: a derive declared through a dependency's
  macro (stylo's `MallocSizeOf`), `smallvec::smallvec!` by path, enums with hundreds of variants
  (stylo's property enums), and 41 NEON intrinsics for the renderer's SIMD layer.
- On Linux a bundled static library (sqlite) is linked from the build script that made it, not
  from a system copy of the same name.

In the pin since OctoSense-org/makepad#138, #139 and #140, all in the JIT compiler and its std
(a cargo build does not use them):

- Every compile error with an error code that the runner's log listed on macOS arm64 is gone
  (371 lines, then none).
  They were bodies in dependencies that the JIT could not compile and would have trapped in if
  the app reached them: missing std items (formatting and `Debug` impls, `Wrapping`,
  `NonZero`, `Cursor`, `Pin`, `ControlFlow`, `OsString` and `Cow` collection impls, integer
  shifts by reference, 22 more NEON intrinsics for moxcms, ...) and ten compiler faults (a
  false `#[cfg]` on a macro's expression fragment, as in ruma's `required_client_scopes`; a
  nested `match` whose arms differ only by a deref coercion, as in stylo; operators on 128-bit
  integers through references; range bounds and tuple indexes; a `switch` with more than 255
  targets, which is what made the 300-variant enum test take 69 GB).
- `stringify!` of a macro's expression fragment no longer prints parentheses around it inside
  a fn the macro made (`"(1 + 2)"` for `1 + 2`): a run-time difference from the cargo build.
- Method lookup follows rustc more closely: a method of a trait that is not in scope no longer
  resolves through a bound on an associated or `impl Trait` type. Code that compiled under the
  JIT only because of that now gets E0599 from `relax check`, as from cargo, until it adds the
  `use`.
- Linux x86_64: the runner no longer stops in makepad's own crates. jetscii's SSE4.2 string
  compares exist now, and the other three faults no longer appear.
- #140, needed since `main` moved to matrix-rust-sdk 61905c3: a `cfg_select!` arm written
  without braces (`pred => expr,`) is parsed as one expression in any position. Before, the
  SDK's `send_queue/progress.rs` stopped the load ("expected `;` or `}` after expression"), so
  the app did not start under the JIT at all. And `Result::flatten`, which the SDK's SQLite
  crypto store calls, exists in the JIT's std (with `Result::unwrap_err_unchecked` and
  `map_or_default`).

## What the port changed in the app

Marked `JIT PORT` in the source.

- System browser rebuilt on upstream native layers (`src/shared/system_browser.rs`).
- Dock state serde without LiveId serde (`src/dock_serde.rs`); Cx helpers (`src/jit_compat.rs`).
- Window activation through `WindowRef::set_visible(true)`; theme sheet loading; trusted VM call.
- Splash sandbox: heap cap, host policy, instruction budget and time budget set as before;
  mini-app isolates end an evaluation at an uncaught error (`set_splash_bail_on_uncaught_error`).
- `octosense-module` builds against the JIT-line module contract (`ModuleWindows`,
  self-confirmed tools) with no change to `src/module.rs`.
- Apple fonts install over every base-theme build (`apple_fonts::install_over_theme_builds`).
- Script tasks drain through `cx.handle_script_tasks()`.
- Shadowed `select` / `form`, tab bar and tooltip signatures, Html `Rc` doc, article-makepad
  text types, `scroll` child renamed `sheet_scroll`.
- `hagency` crate `#![recursion_limit = "1024"]`: relax spends more depth per nested future.

## Gaps against the fork build

- System browser: no navigation policy (`spawn` cannot pin a card to one document), no JS
  bridge, no WebView2 data directory. The fork had these on Windows only, so on macOS the two
  builds behave the same; a Windows build of this line would lose them.
- `Cx::show_notification` is a no-op. On the fork it did something on Android only.
- AES, PMULL and CRC intrinsics are scalar under the JIT: correct, slower than native. A cargo
  build is not affected.
- On Linux x86_64 the runner's log still lists compile errors for bodies it checks ahead of
  use (486 lines): 306 SSE, AVX and AVX2 intrinsics the JIT's std does not have, named by
  fearless_simd's x86 wrapper modules, moxcms's x86 colour conversions and safe_arch. A body
  that uses one traps if the app reaches it. The HTML/CSS preview's renderer draws through
  fearless_simd, and the preview was not opened under the JIT on Linux. On macOS arm64 the log
  lists none of these. They are JIT std gaps; a cargo build is not affected.
- One body the JIT does not compile on any system: `TypeNe::with_fn` in typewit 1.14.2. The log
  line is "macro `projected_type_cmp!` is not supported" (it has no error code, so the counts
  of 10 Nov did not include it). typewit defines that `macro_rules!` through another macro's
  expansion further down the module and brings it into scope with `use`; rustc resolves the
  earlier call through the `use`, and the JIT's parser expands in source order. The body traps
  if called. No run or test of the app reaches it.
- Mobile packaging: `.github/actions/setup-cargo-makepad` reads a makepad git revision from
  `Cargo.lock`, and on this line makepad is a path checkout, so the iOS and Android jobs need
  their own port.

## Not verified

- Linux and Windows beyond what "Platforms the build was run on" lists: no signed-in run on
  Windows, Linux arm64 or Intel macOS, and `html_preview` was built and run on macOS arm64 only.
- The JIT runner on Windows, and on Linux anything past the sign-in screen (see the same
  section).
- iOS and Android: not built (see the mobile packaging gap above).
- Release packaging (`release.yml`, `release-verify.yml`) on this line.
- Hagency services behind sign-in (owner daemon, Pasion, Agents): the test front serves only
  discovery and OAuth, so the app shows "Could not confirm permission status; retrying."
- TLS under the JIT was exercised only by one HTTPS request through the Matrix SDK's client
  (200 over HTTP/2); the test homeserver itself is plain HTTP.
- LaunchAgent install from a JIT run.
