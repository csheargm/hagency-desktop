# Desktop credential storage (R03)

Desktop ports the credential ownership and recovery design from
[hagency-client #22](https://github.com/hagency-org/hagency-client/pull/22).
The Matrix SDK remains the official revision pinned by Desktop; no SDK fork or
new authentication/refresh owner is introduced.

## Storage and migration

On macOS, Windows and Linux, the explicit native adapters use Keychain,
Credential Manager and Secret Service respectively. The service namespace is
`org.hagency.desktop.credentials.v1`; missing, locked, denied or unavailable
native storage fails closed. The installation ID and secret references are
nonsecret. Original database passphrases and document wrapping keys live in the
native store. Session and revoke-only documents use authenticated encryption
through the pinned SDK's StoreCipher. Public identity fields are checked against
the authenticated payload before granting a session.

Legacy session and retained-database files migrate through journal-owned native
references, verified write/readback, private candidate files and a locked
compare-before-replace commit. Preserve the exact original database key, Matrix
user/device, and crypto/state stores. An uncertain commit is not success or
permission to replace the key. A missing key for a published encrypted document
never triggers automatic regeneration. No migration journal contains legacy
secret bytes. Historical backups can still contain their original plaintext;
migration cannot erase external snapshots or guarantee physical disk erasure.

The application-level read-only inspector is adapted from the Client SDK patch
for the official SDK's schema 19. It checks the original account using one
read-only SQLite snapshot, respects committed WAL, bounds input/KDF cost and
fails on unknown schemas. It never calls normal store initialization, migrations
or checkpointing. SQLite may update reader coordination sidecars. Original file
and directory identity are checked before and after inspection. The pinned SDK
writer supplies the interoperability fixtures.

## Authentication lifecycle

Native readiness is checked before opening browser authorization. A memory-only
OAuth candidate does not restore old refresh credentials; expected user/device
are checked, and the original encrypted database is selected only after identity
verification. Synchronous SDK save callbacks retain the actual refreshed pair in
its original owner slot before filesystem work. Cancellation, account changes,
late completion and worker shutdown retain original custody. The bounded worker
performs native operations without blocking the UI thread; timeouts do not mean
an OS prompt or native call was cancelled.

Logout records durable encrypted revoke-only intent before local retirement.
Remote revocation is retried without refreshing tokens or following redirects.
Local success and remotely confirmed revocation have separate UI states.
Desktop's existing native executor retirement and checked stop remain intact.

Automatic startup database deletion is deferred: a snapshot of session references
cannot serialize with another process creating a database. Old native-key garbage
collection is also deferred until unreferenced status can be established safely.

## Verification and limits

Run root library regressions with `HAGENCY_NATIVE_CREDENTIAL_IO=deny`; fixtures
inject a SecretStore and isolate Desktop profiles. Native-vault tests cover
scope/integrity, readback failures, journals, replacement and worker cancellation.
Application tests cover legacy migration, retained store/key continuity, OAuth
round trips, refresh/account retirement, revoke-only recovery and logout UI.

An optional macOS-only native test creates and removes exactly one randomly
scoped qualification entry:

```sh
HAGENCY_NATIVE_CREDENTIAL_IO=allow cargo test --locked \
  --manifest-path crates/hagency-client/Cargo.toml -p hagency-store --lib \
  credential_vault::tests::credential_vault_native_macos_roundtrip -- --ignored --exact
```

Cross-platform native adapter compilation and deterministic tests do not establish
actual Windows/Linux service availability or production OAuth qualification.
Mobile keeps the explicit legacy adapter and refuses encrypted-desktop downgrade.
This change addresses Desktop Matrix session/retained-store credentials; it does
not qualify every separately embedded Agent/runtime secret store. OS credential
storage protects copied profile files; it does not provide complete protection
against a compromised process running as the logged-in user.

The catalog update includes the already-recorded R14 missing invite-permission
translation so the complete catalog gate can run unchanged. No invite policy or
UI behavior is changed by that catalog-only correction.

## Local qualification, 2026-11-11

- Desktop library: 906 passed, zero failed, two existing ignored; native IO denied.
- Credential-focused application selection: 43 passed, including the original
  unrelated URL-credential case matched by the selector.
- Vault selection in the native library workspace: 17 passed; the opt-in native
  test remains ignored by ordinary automation.
- Separate real macOS Keychain qualification: one passed, including removal and
  a subsequent Missing result for the exact randomly scoped entry.
- Locked native binary build and minimal `agent_chat` configuration: passed.
- Catalog, theme, new-module formatting and diff whitespace checks: passed.
- Same local RustSec database as the original review: 27 vulnerability matches,
  with no new advisory/package/version match. This remains the separate R02 debt;
  the audit does not pass and no exploitability claim is made here.

The combined regression gate is recorded in the pull request after completion.
Standalone visible-window startup remains the separate existing PR #55; this
change does not duplicate that entry-point repair or claim release UI acceptance.

Hosted build qualification is blocked by pre-existing warning-as-error failures
in unchanged dashboard and native executor code. Both Ubuntu jobs of PR #57
report the same five unused declarations already failing on PR #55
(`c22a975e`, whose only change from main is the entry-point wrapper). Compare
[the baseline Ubuntu build](https://github.com/hagency-org/hagency-desktop/actions/runs/43002175755/job/130460721913)
with [this PR's Ubuntu build](https://github.com/hagency-org/hagency-desktop/actions/runs/43010833409/job/130489722412).
The warning policy is not relaxed. An unused history-only helper from the Client
port was removed from both adapters instead of adding another dead-code warning.
