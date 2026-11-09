# Hagency workspace and widget UX

## Current candidate — 2026-10-28

Hagency now opens through three task workspaces. Agent manager owns Agent
requests, operation and usage; resource Owner also coordinates supply and owns
capacity/permission decisions; server Admin owns resource admission and server
management. There is no separate project creation or project-admin entry.
The current Matrix account stays signed in while switching workspaces. Its last
workspace is saved in its account-scoped draft. Choosing an entry never grants
server permission, and an account without admission permission cannot enter Admin.

Owner resource requests and Admin admission requests use separate queries and
lists. Manager lists show the account's own Agents; Owner lists show requests for
its own resources. Unknown command IDs, payloads and existing room drafts are
preserved across workspace switches. The pending-command recovery path remains
available because changing navigation must not hide an unfinished operation.

The shared presentation covers the management bundle's 87 page keys, including
hidden legacy compatibility states that are not restored to navigation. The
current controls include eight exact number editors, 46 choice controls, 39 form
groups, ten SVG disclosures, three stateful switches and two progress meters.
Pool applications additionally use the bounded slider and percentage presets.
Trusted native capacity/migration confirmations use the same number editor.
All controls inherit Desktop colors/fonts; cards, fields and the page have
separate surfaces. Text controls have a minimum 44-point height and grow with
the theme's font/control metrics. Advanced options retain their draft when collapsed.

Pool requests are checked against the published limit and a known device limit,
then checked again before room creation or submission. Unknown remaining capacity
is not interpreted as a free balance. An existing over-limit draft is shown with
its original amount and a field error. Owner-defined quantities use explicit
100,000-token steps plus exact typing, without inventing a server ceiling.
Owner top-ups retain the single-confirmation workflow.

Evidence and the author review live in
`.run/hagency-widgets-20261028/validation.json`. Native tests use the offline
`hagency_visual` host and explicit fixtures; they do not authorize or perform
live approvals, budget increases, room creation or executor changes. See the
packet for the exact source/build hashes and accepted runs. The numerical review
is an author assessment of representative core paths, not an independent user
study or proof that every historical page/state has been exercised natively.
Existing extracted-fixture failures are compared against the pre-change source;
they are not counted as passing tests. The combined budget/chat regression gate
is required before a local Desktop handoff.

## Historical evidence

The sections below describe older implementations and their original evidence.
Their source hashes, role names and scores are not acceptance of this candidate.


## Management visual refinement — October 2026

The current pass adds a theme-derived gradient banner, a connected-resource SVG
mark, four illustrated setup steps, section badges, a persistent accent-colored
navigation selection, selected capability filters, and distinct card/action
surfaces. Secondary actions use lighter borders. The scrollbar has its own
gutter and reads the shared theme colors. English and Chinese labels remain in
the bundle; no separate palette or font family is introduced.

All state, request, recovery and permission helpers before `A.render_page` are
byte-identical to the pre-redesign source. Resource amounts retain their original
unknown/zero semantics. The workflow illustration explains the sequence; it
does not assert completed steps or invent resource totals.

The native review uses `examples/hagency_visual.rs`, the complete shipped Splash
source, and `tests/visual-read-fixtures.json`. The host has no Matrix client,
management backend or executor; undeclared requests fail closed. Its root-layout
retention and next-frame reply redraw match the production mini-app host. Build
the example with the repository's normal release features, then launch with a
fresh `HAGENCY_DESKTOP_DATA_DIR`, an absolute `HAGENCY_UX_FIXTURES` path and
`MAKEPAD_HIDE_WINDOWS=1`; `--narrow` selects a 430-point window. The instrument's
`/event?data=ux:light`, `ux:dark` and `ux:violet` apply real Rinx themes. End owned
instances with `/quit`.

Evidence in `.run/hagency-ux-polish/validation.json` binds bundle
`61174988f5ac8b7591627922f57a2cff5d36a44a85727183d6e570b74d95e111`
to the release build and captures. All three native journeys passed: English
1000×900, Chinese 1000×900, and English 430×850. They exercise filters, exact
resource detail, editing/cancelling a local-name draft, preserving typed EN/CN
text through theme reapply, returning with the filter retained, and empty
Projects/Inbox states. Teal light/dark and violet appearance were captured.
Codex authored the change, drove the instrument and inspected the screenshots;
these are offline native pixels, not live-service E2E or phone acceptance. No
new numeric UX score is assigned.

The final bundle/root, locale, web-management, refresh and connection suites pass
222 checks; the system-app catalog passes 8 checks; the macOS release build
succeeds. Four older extracted-fixture suites still fail with missing fixture
helpers/properties. Running them against the pre-edit source produces identical
per-case failures, recorded in `scope-check.json`; this is not a claim that the
entire repository test suite passes. Initial preview-host and test-selector
failures are retained separately from the corrected native run. Publication and
the earlier role-specific 9/10 review campaign are not re-certified by this pass.

## Native baseline

The full native captures in `.run/management-project-e2e-3/captures` use bundle
`67d3423100e4402f013093e0fed991175cf7306f684bbe3bd4883c413ed780f4` and desktop
binary `208a5a60cb520f680472f4ea8ff09d4c04db4093a748067f527b05ba84ee611d`.
All five project-flow checks passed with zero script errors. The unchanged
visual scorer gave the coordinator's Chinese dark view 8.55/10, with full
contrast/safe-area/palette marks and lower alignment, rhythm and content marks.
The English captures scored 9.36–9.59. These are baseline results, not acceptance
of the changes below.

Visual inspection finds that the current stage, explanatory copy and role
details have nearly the same emphasis. The connection link is separated from
the resource it opens. The connection page has no direct route back to that
project. Contrast already passes; changing shared colors or adding decorative
content to improve a score would not address these issues.

## Detail-view changes

| Change | User-facing purpose |
| --- | --- |
| Body-size bold section headings, shared 8/16 spacing | Distinguish status, request details and people without enlarging ordinary text or introducing a second theme. |
| Approval scope expressed as capacity and agent facts | Explain that approving the request does not reserve capacity or create an agent. Do not infer a native balance from the project-intent response. Keep allocation and setup explicitly unavailable. |
| Resource identity, connection action and coordinator guidance in one group | Explain what the coordinator can inspect and that the enrolled resource owner performs verification. |
| Contextual return from connection to project | Keep a transient originating project ID; fetch the latest project on return. Clear the origin on normal navigation. |

All labels and guidance remain English/Chinese. Components inherit Rinx theme
colors, font sizes and SVG handles. The change adds no capability, backend
mutation, capacity claim, allocation control or agent action. Pending commands,
drafts and approval authority retain their existing behavior.

The implemented bundle is
`266e3617ebda4ca0d8f35cb7cd71b4f908d745998e94ced782b7cdb69cc5d9dd`.
The pinned Makepad `1f3b1de` parser, previous compatibility helpers, new route
helpers, package stamp and system catalog checks pass. Only the three navigation
helpers changed; mutation, draft and recovery helpers remain byte-identical to
the baseline. Check details are in `.run/miniapp-project-ui-hierarchy-check.json`.
The native run `.run/management-project-e2e-4/report.json` now passes six
checks with zero script errors. All six full-window captures were inspected.
Chinese dark project detail scores 9.17 and English screens score 9.37–9.66,
but Chinese dark connection detail scores 8.47, so the overall gate remains open.
The same six checks also pass against the upgraded schema-6 localhost server
in `.run/management-schema6-project-e2e-1/`; all six new captures were inspected.

The next bundle, `b18c828ddfb0f537f42d1e9e581af1b8bc951e09c4bc68128e57117567c56e09`,
separates server admission from the expiring executor check into aligned rows,
uses the existing section hierarchy for the next action, and shows the actual
originating project below its return control. Only the rendering helper changes.
Its parser, i18n, package and catalog checks pass. The targeted native run
`.run/management-connection-detail-e2e-1/report.json` passes three checks with
zero script errors using release binary `4f87e94f…`; all three full-window
captures were inspected. Owner English/light scores 9.60, coordinator Chinese/dark
direct connection scores 9.00, and the project-context connection scores 8.99.
The last view still fails the 9/10 threshold; the gate remains open.

The following horizontal-context revision, bundle `878fca79…` in release
`4f8f99e2…`, also passes the three native checks with zero script errors
(`.run/management-connection-detail-e2e-2/`). All captures were viewed. Its scores
are 9.60 English/light, 9.00 Chinese/dark direct connection and 8.93 contextual
connection. The horizontal row alone does not pass the requested threshold;
the original captures and lower score are retained for comparison.

The next header-group revision, bundle `2500d3dd…` in release `9aa788ba…`, passes
the three connection checks and six project regression checks with zero script
errors. All nine captures were viewed. Connection scores remain 9.60, 9.00 and
8.93 respectively (`.run/management-connection-detail-e2e-3/`); the contextual
screen still fails the requested threshold. The functional result does not close
the visual gate or the remaining allocation/agent workflow acceptance.

## Validation required after editing

Run the existing Splash parser/helper checks, restamp the package, and check the
system-app catalog. Then rebuild the native app and capture the complete window
in English/light and Chinese/dark. Inspect the project detail, resource
drilldown, return-to-project, ordinary navigation, refresh and theme reload.
Returning must show current server data; normal navigation must remove the
contextual return link. Re-run the existing five role/decision checks and the
unchanged visual scorer. Do not mark the 9/10 gate passed before those captures.

## Project setup workflow

Version 0.3.0, bundle
`aec4a729e6cfae2a6159d39aeb7b113ed10ae3c1403547ce1f3e31fb1e52c166`,
adds the setup screen only when the server advertises `project-setup-v1`.
It separates the current reservation, assigned administrator, original-device
space/room creation stages and recorded Matrix verification. A returned room ID
is not presented as a verified room, and absent capacity is not shown as zero.
Setup history remains reachable after a project is withdrawn.

The server's current role flags govern assignment, begin, cancellation and
rebasing. Begin and rebase use the exact eligible reservation ID and revisions.
Continue additionally requires the original local creation journal. Every
mutation has a confirmation; recovery retains the original native command ID.
After an error the screen clears stale progress and requires refresh/recovery
before another operation. Cancellation explains that existing/in-flight rooms
and resource holds remain. Setup does not create or start an agent.

The package declares exactly eight services used by this screen: progress,
begin, continue, cancel, rebase, retry, acknowledgement and administrator
assignment. The unused `setup.resume` service is not granted. Native recovery
also checks the original operation's permission; a retry grant alone is not
mutation authority.

The pinned parser and extracted helper VM checks pass, including old-server
feature absence, exact reservation selection, original-ID recovery, revoked
project navigation and role/local-history gates. Package policy, the digest
stamp and eight catalog checks pass. All 12 Desktop host regressions pass in
`.run/hagency-setup-ui-host-tests.log`, including the exact eight shipped setup
grants and retired manager recovery. Combined native strict Clippy passes in the
client's `.run/journal-profile-clippy-final.log`. Actual native EN/CN role,
restart, confirmation and pending-command tests remain required; this checkpoint
does not claim native visual acceptance or end-to-end creation against a real
reserved resource.

The release-7 native setup run now passes six workflow checks with no script
errors (`.run/management-project-setup-e2e-1/`), including role restrictions,
assignment confirmation/cancellation, restart and withdrawn-project history.
All five captures were inspected. English manager and confirmation screens score
9.32 and 9.51, but Chinese dark coordinator scores 8.38, so visual acceptance is
still open. These tests do not exercise room creation with a real reservation.

A separate native regression finds that the Matrix-rooms creation status was
missing. The pinned VM reproducer in `tests/fixtures` isolates a skipped named
input branch corrupting object construction. The corrected subtree gives
Creation its own aligned row and scopes conditional recovery inputs inside
sections; the six empty/remote/local/mixed states pass in both languages,
including preserved input IDs and callbacks. An empty action container is no
longer emitted for read-only or blocked states. Helpers, permissions and native
operation mapping are unchanged. Native rendering and the visual gate must be
rechecked against a newly stamped build; release-7 evidence is retained.
