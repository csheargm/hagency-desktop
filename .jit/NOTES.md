# Hagency Desktop on the makepad JIT line: port notes

The desktop app (package `rinx`) builds against the makepad JIT line: upstream makepad `work`
(the HAL rewrite with the relax JIT) plus the fixes on OctoSense-org/makepad branch `hagency-jit`.
It runs two ways from the same sources:

- under the JIT runner, `makepad run`, where an edit is patched into the running app in about
  0.1 s instead of a 10–37 s cargo rebuild and restart;
- as a plain `cargo build`, which is what CI and release packaging use.

STATUS 10 Nov 2026: runs signed in, `html_preview` included. The JIT run is checked on an Apple
silicon Mac; the cargo build has also been run on Linux x86_64 and arm64, Intel macOS and Windows
(see "Platforms the build was run on").

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
    $relax test rinx --lib --allow-c -- --test-threads=4       # the app's unit tests under the JIT

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

A JIT compiler fault can take tens of GB before it stops (a single-file test with
`#[derive(Debug)]` on a 300-variant enum reached 69 GB). On a shared machine run `relax` and
`makepad run` experiments under a memory limit.

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

The JIT runner itself (`makepad run`) was run on macOS arm64 only. On Linux x86_64 it builds and
then stops in makepad's own crates (jetscii's SSE4.2 imports, a const-evaluation cycle in
`makepad_hal::ledger::Pool::ALL`, a `UniformBufferBindings` layout mismatch, a
`ScriptTypeObject` field). It was not tried on Windows.

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
- The runner's log lists compile errors for bodies it checks ahead of use (211 lines on 10 Nov):
  moxcms's NEON colour conversions, which miss 22 intrinsics, and a few `Debug` / `Display`
  impls and admin endpoints in serde_json, zerovec, deranged, cap-std and ruma. None was
  reached in the runs above, and nothing in the app asks `image` for a colour-space conversion,
  which is what would reach moxcms. They are JIT compiler gaps; a cargo build is not affected.
- Mobile packaging: `.github/actions/setup-cargo-makepad` reads a makepad git revision from
  `Cargo.lock`, and on this line makepad is a path checkout, so the iOS and Android jobs need
  their own port.

## Not verified

- Linux and Windows beyond what "Platforms the build was run on" lists: no signed-in run on
  Windows, Linux arm64 or Intel macOS, and `html_preview` was built and run on macOS arm64 only.
- The JIT runner on Linux and Windows (see the same section).
- iOS and Android: not built (see the mobile packaging gap above).
- Release packaging (`release.yml`, `release-verify.yml`) on this line.
- Hagency services behind sign-in (owner daemon, Pasion, Agents): the test front serves only
  discovery and OAuth, so the app shows "Could not confirm permission status; retrying."
- TLS under the JIT was exercised only by one HTTPS request through the Matrix SDK's client
  (200 over HTTP/2); the test homeserver itself is plain HTTP.
- LaunchAgent install from a JIT run.
