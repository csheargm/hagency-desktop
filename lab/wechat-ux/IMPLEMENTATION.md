# Matrix and four-tab native production pass

The agreed [production scope](SCOPE.md) covers Matrix messaging, four native
mobile tabs and live Palpo testing. Broader WeChat parity is separate. This
document records implementation and runtime evidence; visual acceptance remains
unpassed under the unchanged reference gate.

The scoped implementation and runtime checks pass. Physical-device release
qualification and the broader visual parity work remain separate, as described
below; this is not a 9/10 similarity receipt.

Implemented so far:

- Four mobile roots: Chats, Contacts, Discover, Me; desktop navigation remains.
- One mobile chat list combining direct and group rooms in SDK order, square
  avatars, compact previews, unread badges, filtering and native room selection.
- Contacts from Matrix direct-room targets, directory/full-user-ID search,
  profile viewing, the existing confirmed direct-message creation flow, and a
  dedicated joined-group list (excluding direct rooms and spaces).
- Me uses the signed-in Matrix profile and opens existing account/settings UI.
- Mobile settings has a native list of Account and Security, General, Privacy,
  and About; each opens the existing production settings controls.
- Mobile text-message templates with incoming/outgoing bubbles, font-measured
  bubble widths, and the existing message actions, replies, reactions and status.
- Compact mobile composer retaining the existing native Matrix send controller.
- Compact gray reply quotes below mobile bubbles, with a separate composer
  preview; long quotes retain expansion, collapse and jump-to-original behavior.
- Mobile Chat Info opens from the conversation header and reads real room members,
  names, avatars and topic. It connects to the existing invite modal and Matrix
  notification controls. Returning preserves the room's timeline and draft.
- Room notification choices call Matrix push rules and report success only
  after the server confirms. All four modes passed native/backend checks.
  Muted indicators and current-choice marks follow sync changes, including
  updates received while the menu is open, and survive restart.
- Incoming/outgoing native image templates use bounded widths and heights;
  Matrix-loaded avatars work in Chats, Contacts, profiles and Me.
- Absolute `HAGENCY_DESKTOP_DATA_DIR` override for an isolated native test profile and cache.

The native probe is intentionally strict: first-open blank pages, script errors,
and messages that fail to reach a Matrix recipient fail the run. Debug captures
and failed runs in `evidence/live/` are development evidence, not acceptance.

## Live server

Mini 1 hosts a separate `robrix-mobile-soak` Docker network, homeserver and
PostgreSQL container. The homeserver binds only `127.0.0.1:18120`; access from
the developer machine uses SSH forwarding. Existing Palpo deployments are not
changed. Test storage is under `~/robrix-mobile-soak/` on the host because the
Colima VM disk was full. Credentials remain in private files; never commit them.

The tested image is `palpo-url-cas:056f5966f5b186d6`, reporting Palpo 0.4.0 and
support for simplified sliding sync. `seed.py` creates four isolated test users,
three direct rooms and two group rooms. All sample messages belong to those users.

```sh
export ROBRIX_TEST_SSH="user@your-isolated-test-host"
ssh "$ROBRIX_TEST_SSH" python3 - < tools/wechat-ux/live/provision_palpo.py
ssh -N -L 18120:127.0.0.1:18120 "$ROBRIX_TEST_SSH"
python3 tools/wechat-ux/live/seed.py
cargo build --locked --features agent_chat
python3 tools/wechat-ux/live/native_probe.py
python3 tools/wechat-ux/live/soak.py --minutes 30
python3 tools/wechat-ux/live/native_soak.py --minutes 30
python3 tools/wechat-ux/live/native_reconnect.py
python3 tools/wechat-ux/live/native_notifications.py
python3 tools/wechat-ux/live/native_actions.py
python3 tools/wechat-ux/live/native_quotes.py
python3 tools/wechat-ux/live/native_chat_info.py
python3 tools/wechat-ux/live/native_layouts.py
python3 tools/wechat-ux/live/seed_media.py --mock-root /path/to/makepad_wechat
python3 tools/wechat-ux/live/native_media.py --mock-root /path/to/makepad_wechat
python3 tools/wechat-ux/live/review_native.py
```

`native_probe.py` launches and owns one native client, drives actual hit testing
and text input, captures PNGs, verifies sends from the recipient's server history,
and shuts its client down. `soak.py` separately exercises server durability and
delivery over time. A server-only soak is not evidence that the native UI passed.

The first live smoke passed all four roots, a contact profile, native composition
and recipient-confirmed delivery. The server-only run completed 535 iterations
and 2,355 checks in 1,800 seconds with zero failures; p95 iteration time was 0.616
seconds. The native run `49a2c4886b09` passed 94 complete cycles in 1,809 seconds,
including draft preservation, recipient-confirmed sends and rendered replies.
Its binary hash is recorded in the JSONL receipt; later presentation/settings
changes are being validated separately. Earlier attempts exposed OCR
ambiguity and a test that sent input before the pushed screen had drawn; their
failed receipts remain in the evidence directory. The runner now checks the
actual composer pixels and exact delivered draft content separately.

Builds pass with `--locked --features agent_chat` and default-feature checking;
133 Rust library tests and 26 evidence-validator tests pass.
These checks do not grant a similarity score. The native run
records its binary hash, so subsequent UI edits need fresh native validation.
New probe runs retain immutable copies under `evidence/live/native-runs/`.
`native_reconnect.py` creates a separate profile behind an owned loopback proxy;
its interruption affects only that test client and never stops the Palpo server.
That test passed: the blocked native send was absent from recipient history,
then arrived exactly once after transport recovery. Native layout checks passed
at 320×640, 375×812, 430×932 and desktop 1024×768. They check visible tab bounds
and the rendered selected color; they do not establish device/IME behavior.

`native_notifications.py` passed external-session mute, restart persistence and
external-session reset while the choice menu remained open. An earlier failure
found that the menu held an old snapshot even after the row state updated; the
menu now consumes the confirmed state update. The row's muted icon was also
inspected in the native capture. This does not test OS push delivery.

`seed_media.py` optionally uploads the public mock's sample images only to the
isolated test accounts. It records source commit, asset hashes and event IDs in
the ignored evidence directory; the imagery is not bundled into Robrix. The
retained upstream license is Apache-2.0. `native_media.py` checks the rendered
photo pixels and viewer return, separately from native file-picker/upload tests.

`native_actions.py` verifies the recipient's exact reply relationship, edit
replacement and reaction target, plus rejection of an empty reaction. The live
probe found first-open focus bugs in the edit pane and reaction input: both now
request focus after drawing their input area. A rejected empty Enter keeps the
reaction input focused. Mobile edited labels now fit the avatar column.

`native_quotes.py` passed four checks: quote placement/collapse, explicit expansion,
tap-to-expand and tap-to-original. The first test attempt assumed expansion would
automatically scroll; the corrected runner scrolls to the expanded quote's end.
`native_chat_info.py` passed eight checks, including server-confirmed notification
changes, invite cancellation, draft and scroll-anchor preservation, reopening and replacing direct
room members with the selected group's members. Earlier failures exposed a
released parent timeline and duplicate actions from a list row. Each row click
is now processed once. A subsequent soak completed 17 functional cycles but
failed its strict draw-error check. Chat Info now restores the room through the
normal saved timeline/draft lifecycle instead of keeping a released view live.
Failed receipts remain available alongside passing runs.

The corrected navigation passed a fresh five-minute soak (`611334a4423c`):
17 complete cycles in 312.6 seconds, with no draw errors. Each cycle included
all four roots, Chat Info, draft restoration, a recipient-confirmed native send
and a rendered peer reply. This is a regression run for the updated build,
separate from the earlier 30-minute native and server soaks.

## Still required

The latest focused checks and navigation soak pass. Native uploads, account
switching, permissions, locale, real device keyboards and broader scroll
restoration still need their own release qualification. The backend gaps in
`api-map.json` for Moments, payments, mini programs and calls belong to the
separate parity workstream, together with the complete screenshot/journey review
under the unchanged gate. The two completed 30-minute soaks and later focused
checks cover their recorded builds and paths. No similarity score or unrestricted
mobile release verdict has been awarded.

## Follow-up: Apple fonts and web mini-app cards

PingFang SC Regular/Semibold now covers English and Chinese across the Apple
theme, with bundled fallback. Three font unit tests and four macOS native
render/send checks pass. The iOS implementation uses the same CoreText APIs;
iOS compilation/device checks remain unavailable on this machine.

[Web mini-app cards](MINI-APPS.md) are available from **+ → Share mini app**.
Six native checks passed, including actual WKWebView pixels/JavaScript input,
web navigation, exact Palpo delivery, cross-chat sharing and incoming cards.
Six separate headless Makepad checks passed, including a verified hidden window,
WebKit JavaScript load/reload callbacks and recipient-confirmed sharing. The
mobile attachment menu also now stays within the viewport. These results use
their own build hashes in `validation-summary.json`; earlier soak receipts
continue to identify their original binaries. No Matrix-spec completeness or
9/10 visual parity claim follows from these results.
