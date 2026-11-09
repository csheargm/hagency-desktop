# Moments and private File Transfer

Implements the first slice of [ADR 0001](../../docs/adr/0001-matrix-moments.md).
The user authorized implementation after the architecture discussion. This is
separate from the full WeChat visual-similarity acceptance gate.

## Use

- **Discover → Moments** combines joined author timelines. Open a post to like,
  comment, edit your own text, or delete your own contribution.
- The desktop sidebar's globe opens the same Moments feed.
- **Me → My Posts** shows your publishing history. **Post** opens the composer.
  **Audience** manages explicit invitations and removes viewers.
- **Contact Info → Moments** shows that person's accessible posts.
- **Invites** shows named Moments invitations and requires an explicit join.
- **Hide author** removes their posts from your combined feed. **Audience →
  Show author in feed** reverses it without changing any room membership.
- **Chats → File Transfer** opens a separate encrypted chat containing only
  your account. It supports the existing message/file composer across devices.
- In message-history details, **Post to Moments** opens a separate composer for
  your own text. It does not publish until you press Post and review its audience.

One audience applies to an author's entire timeline. Members can see each other,
comments, and likes. This does not implement WeChat's mutual-friend visibility
or per-post privacy lists. Posting to yourself in File Transfer never publishes
to Moments, and adding timeline viewers never changes File Transfer membership.

## Matrix contract

`m.room.create.type` is `rs.robius.robrix.moments`. The immutable create-event
sender is the publishing author. A name, alias, JSON author field, Space, or DM
is not an ownership signal. Creation configures encryption, invite-only access,
joined history visibility, private directory visibility, and author-controlled
state/invitations. SDK encryption state must also be available before sending.

A text post uses this content inside an encrypted `m.room.message`:

```json
{
  "msgtype": "m.text",
  "body": "A quiet afternoon",
  "rs.robius.robrix.moments": {"version": 1, "media": []}
}
```

An album adds up to nine ordered `media` entries with `name`, `mimetype`, `size`,
and a standard Matrix encrypted `file` object. Each asset is at most 25 MiB.
The root uses `m.image` or `m.video` and the first asset as its ordinary-client
fallback. One root event identifies the whole album. Video files are available
through Download; this slice does not add a new in-feed video player.

Comments use `m.thread` with the root event ID. Likes use `m.reaction` with the
`❤️` annotation key. Reactions normally are **not encrypted**. Likes deduplicate
by sender/key; unliking removes loaded duplicate annotations. Synapse's
`M_DUPLICATE_ANNOTATION` response is reconciled with the existing reaction.

Edits retain the original event identity and must come from the original sender.
Redaction removes visible content. It does not erase copies someone downloaded.
Non-owner roots, cross-room relationships, and unsupported versions cannot
become valid author posts. Matrix permissions do not distinguish posts from
comments inside encrypted events; authorship validation is a client rule.

## Persistence, loading, and privacy

- Account data remembers room choices, hidden authors, and seen post IDs. It
  contains no post bodies or media encryption keys.
- Room creation records an operation ID and reconciles joined rooms after an
  uncertain response. Multiple owned timelines require a choice; no automatic
  audience merge or deletion occurs. **Audience → Retry timeline setup**
  reconciles again before retrying an incomplete setup.
- The per-account local outbox uses atomic, mode-0600 files, stable transaction
  IDs, confirmed-event IDs, and completed upload records. Retry reuses these.
  Composer drafts and copied picker files stay in the account's local profile.
- Publish rechecks the audience, SDK membership, room type, author, and
  encryption. Audience changes pause a saved send until explicit review.
  Confirmation checks the exact audience displayed; another intervening change
  requires another review. A friend's audience remains read-only.
  Discarding a retry does not unsend an operation already received by the server.
- Viewer removal confirms membership, waits for SDK agreement, and discards the
  outgoing Megolm session. Author posts also rotate that session before sending.
- History fetches are bounded to eight timelines and 50 events per timeline per
  batch. Per-room cursors preserve older pagination and reconnect catch-up.
  Missing-key events remain recoverable. Media loads only for visible cards.
- The open feed refreshes periodically. Account/device and request checks reject
  stale results; logout clears the native panel. Read refreshes do not block
  navigation. Posts have their own New indicators rather than chat unread totals.
  Joining a timeline on another device also replaces cached invitation metadata.
- Typed Moments rooms are excluded from initial and incremental chat routing,
  Contacts groups, Space trees, room-name matching, and forwarding/mini-app
  destinations. Ordinary chat notification-setting updates also exclude them.

## Validation

Verified on 2026-09-20 with native binary SHA-256
`f60b2b584f4823a0307cd73014d89136192c74106e7aaeee49948320906a9efb`:

| Check | Result |
| --- | --- |
| Library tests, `--locked --features agent_chat` | 166 passed; live integration test separately invoked |
| Palpo recipient/encryption integration | 17 checks passed |
| Isolated Synapse recipient/encryption integration | 17 checks passed |
| Native mobile and desktop journeys | 14 checks passed; zero native errors |
| Native build and `git diff --check` | Passed |

The local receipt is `evidence/live/moments/validation-moments.json`, with
per-run binary hashes, input traces, screenshots, and test logs. Native checks
cover explicit invitations, recipient rendering, comments/likes and their
edits/removal, post redaction, hide/unhide, album navigation, separate File
Transfer, My Posts, draft restoration, and desktop navigation. The Synapse
fixture was stopped after testing; its isolated data remains available.

Pure model tests cover ownership, foreign-room relationships, edits/redactions,
missing-key recovery, duplicate likes, unknown versions, permissions, private
self-chat membership, and duplicate-timeline selection.

The opt-in SDK integration test is
[`live_tests.rs`](../../src/moments/live_tests.rs). It accepts only explicitly
supplied `@robrix_ux_` accounts. The same test runs against Palpo and an isolated
Synapse instance. It checks actual recipient decryption on two devices, encrypted
album bytes, outsider access, pre-join keys, removal with retained old keys,
stable retries, partial-upload recovery, edits/reactions, and a real encrypted
file reaching another device of the same account.

```sh
ROBRIX_MOMENTS_FIXTURE=/absolute/path/to/fixture.json \
HAGENCY_DESKTOP_DATA_DIR=/absolute/path/to/isolated-moments-profile \
cargo test --locked --features agent_chat --lib \
  moments::live_tests::palpo_moments_recipient_encryption_and_native_seed \
  -- --ignored --nocapture
python3 tools/wechat-ux/live/native_moments.py
```

The native script uses real Makepad pointer/keyboard input and captures both
sender and recipient clients. Evidence, credentials, and profiles remain in the
ignored `evidence/live/moments/` directory. No personal account is driven by it.

Observed Palpo limitation: after viewer removal, its event endpoint can return
later **ciphertext** to that viewer. The retained-key tests separately verify
that neither removed-viewer device can decrypt the later content. The UI must
not equate HTTP visibility with access to usable encrypted content, or claim
that reaction metadata has the same protection.

Physical iOS devices, OS media-picker dialogs, very large friend networks, and
the full bilingual 9/10 WeChat visual comparison require separate evidence.
