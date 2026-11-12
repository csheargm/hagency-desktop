# Setup room rendering regression

The release-7 native capture omitted the creation status despite successful setup
HTTP/role checks. Evidence is retained at
`.run/management-project-setup-e2e-1/captures/manager-requirements.png` and the
separate native regression `.run/management-setup-status-regression-1/report.json`.

`fixtures/skipped-named-child.splash` reduces the defect in pinned Makepad
`1f3b1de`: a named `:=` child inside a skipped conditional corrupts the enclosing
object construction. Its expected object contains exactly `heading`, `not begun`,
and `verification`; the assertions fail before those children can be inspected.
`fixtures/setup-rooms-release7.splash` preserves the actual original room subtree
with inert widget prototypes and an empty setup response. Neither fixture is
part of the shipped package. The renderer fix scopes each conditional named
input inside a widget object and uses an unconditional, aligned Creation row.

Run the current subtree checks from the Desktop repository:

```sh
cargo build --locked --manifest-path tools/hagency-script-review/Cargo.toml
python3 apps/hagency/tests/check_setup_rooms.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-setup-room-render-check
```

The helper accepts `parse <file>` and `eval <file>`, uses the pinned
`makepad-script` VM, captures diagnostics, and requires a final `true` result.
The test extracts the current Matrix-rooms subtree and stage helpers from
`bundle/main.splash`. It checks six states in English and Chinese: no setup,
remote history, known local room IDs, both unknown stages, and each mixed
known/unknown combination. It asserts status/ID rows and the actual recovery
input IDs and callbacks. The historical failing fixtures are also run and their
exit codes recorded separately, so a future framework fix can be observed.

This is a script-object regression, not native widget or visual acceptance.
Rebuild and run the full native role/restart/status probe, inspect full-window
captures, and keep the unchanged UX threshold. No permission, operation, command
ID, or state-machine changes are made by this rendering fix.

## Resource detail and navigation checks

```sh
python3 apps/hagency/tests/check_resource_detail.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-resource-detail-vm
```

This evaluates the actual resource-detail subtree in English and Chinese for
loading, unavailable, unmeasured, measured, zero and undeclared-limit snapshots.
It checks Back/filter retention, exact-ID reads, clearing old snapshots and
rejecting mismatched results. Every case also checks busy navigation and a
server without project features. The latter needs a widget wrapper around the
conditionally named Projects button to avoid the pinned skipped-child defect.
The original failed reduced navigation case remains in
`.run/hagency-resource-detail-vm-4/no-projects-en.splash`.

Only `hagency.resources.get` is added to the bundle grants. Configuration and
publication writes remain absent. These checks prove script objects and routes,
not native pixels, provider availability or budget authority. Native instrument
captures and long-content/layout acceptance are still required after rebuilding.

After building, the separate native instrument probe uses an already signed-in
loopback fixture. It does not restart, switch accounts, change preferences or
submit a management command:

```sh
python3 tools/hagency/native_management_resource_detail.py \
  --run .run/management-ui-owner --port 8318 \
  --account '@miniapp_owner_1015:localhost:8088' \
  --model gpt-5.6-sol --framework codex --language en \
  --capability coding --output .run/management-resource-detail-native
```

Select an unambiguous model/framework and budget, or pass an explicit zero-based
`--occurrence`. The probe does not claim a hidden resource ID from these public
labels. Omit `--capability` to
retain the existing filter, or run again with `--language zh-CN` in a separately
prepared Chinese fixture. `--expected-budget '123456 / 123456'` selects and asserts a
known fixture value, including after refresh. The probe checks actual detail sections, reachable footer,
no edit controls, refresh, Back/list retention, unchanged saved command and
native error logs; it leaves the signed-in app on Resources. Whole-window
captures still require visual review and the unchanged UX scoring gate. This
probe is authored but has not been run against the new integrated build yet.

## Project agent admission

```sh
python3 apps/hagency/tests/check_agent_admission.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-agent-admission-vm
```

The 36 English/Chinese checks evaluate the actual shipped request/list/detail and
Inbox subtrees plus original-command handlers. They cover missing discovery,
combined manager authority and resource-owner separation, missing capacity, draft validation, explicit
review/confirmation, pending executor status, old receipts without project names,
unknown-result retention and definitive rejection. The empty Inbox fixture also
catches the pinned VM's skipped named pagination-child defect. These are script
objects and callbacks, not native screenshots or a successful running agent.

`tools/hagency/native_management_agent_readiness.py` is a separate read-only
instrument probe for an already-open isolated profile. Use `--expect hidden`
against a server without admission discovery, or `--expect blocked` with an exact
`--expected-blocker` for a real project whose prerequisites are incomplete. It
preserves account/project/original command, captures full windows and never seeds
capacity or changes flags. It is authored and syntax-checked; it has not yet run
against an integrated admission build. Full native manager request→manager
confirmation→latest result acceptance requires genuine qualified capacity and
verified Matrix setup. Schema 22 automatically establishes the manager as the
project authority; no separate project-administrator account or assignment is used.

## Resource identity and status layout checkpoint

```sh
python3 apps/hagency/tests/check_resource_list.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-resource-list-vm
python3 apps/hagency/tests/check_status_layout.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-status-layout-vm
```

Resource cards keep the actual model/framework title. Their ID is abbreviated
only for display (first four and last eight Unicode characters). A collision
among the current page's IDs shows the full IDs instead. The abbreviation is
never an input to a service: detail and connection actions retain the complete
native ID. Detail displays that full ID. The 14 EN/CN list checks cover identical
models/budgets, abbreviation collisions, unknown/zero budgets, Unicode, busy
controls and exact action targets. The server contribution name remains a
separate identity; the list does not infer a resource name from partial connection
history.

The 16 setup/connection checks evaluate whole current page subtrees for role
separation, original-command recovery, confirmation cancellation and contextual
Back navigation. The unchanged pre-edit source also reproduces skipped named
children in these larger subtrees (logs under
`.run/resource-identity-layout/status-baseline`). Conditional named controls now
have a real action/section container, preserving their existing permission gates
and handlers. This records a VM failure, not a newly asserted live-native failure;
the previous integrated native captures passed their functional probes.

The native resource probe now accepts `--resource-id FULL_ID`. It selects the
visible full/short label and requires the full ID on both detail and refresh.
Ambiguous visible cards still fail closed unless explicitly selected. An ordinal
remains supported for earlier builds but does not prove identity. No probe has
been run against this new layout checkpoint; native full-window inspection and
unchanged UX scoring remain required.

## Agent status

```sh
python3 apps/hagency/tests/check_agent_status.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-agent-status-vm
```

These 24 English/Chinese checks evaluate the actual status subtree and handlers
for loading, failed reads, absent results, disabled policy, qualification, stale
checks, unknown outcomes and cleanup. They check exact-target refresh, rejection
of a mismatched response, Back and preservation of the original command. Eight
checks cover project guidance for all setup/admission discovery combinations.
No mutation control or zero-usage inference is allowed. They are script-object
tests, not native pixels or proof of a running agent.

The shared-agent status/parity boundary and remaining work-history, top-up and
removal contracts are recorded in
[`hagency-agent-status-parity.md`](../../../docs/design/hagency-agent-status-parity.md).

`tools/hagency/native_management_agent_status.py` starts on an already-open real
agent request detail. It requires the actual translated stage expectations and
optionally the recorded native agent ID, reads the status, refreshes and returns
to the request. It preserves missing/present draft state and the original command,
captures whole windows and checks native errors. It does not create requests or
seed capacity; an integrated real request is still needed to run it. The probe
is syntax-checked only at this checkpoint.

## Current device setup handoff

```sh
python3 apps/hagency/tests/check_onboarding.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-onboarding-vm
```

The 32 English/Chinese VM checks execute the actual current onboarding subtree
and callbacks. Only `uninitialized` opens device setup; `active` opens provider
accounts and resources. They cover unavailable builds, SDK owner blockers,
original publication reviews, closed/unknown outcomes, rejection of unsupported
status shapes, and preservation of an unrelated saved command. Setup has no
token input, filesystem path, provider identity or script confirmation flag.

Native setup checks the signed-in owner and execution device and persists the
current initialization identity. It creates no provider account, shared quota or
model work. Provider enrollment, server admission and publication remain separate
reviews. These VM checks verify object construction and callbacks, not screenshots
or live account effects. Native restart and current management coverage runs in
the integration gate.

`fixtures/destructure-widget-scope.splash` is a reduced pinned Makepad `1f3b1de`
regression: destructuring an optional field inside a nested object constructor
loses the outer `root` binding. The intended object is one container with one
`uninitialized` text child. This independent language reproducer is not
counted as product acceptance. The shipped helper reads the optional field outside
the constructor; all actual onboarding subtrees must pass.

## Shared-work history

```sh
python3 apps/hagency/tests/check_agent_work.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/agent-work-vm/work
```

The 36 English/Chinese cases evaluate the shipped index/history widget subtree,
nullable/partial/zero/complete accounting, distinct reply state, unknown future
states, old-server support, original request identity, bounded pagination/filter
retention and history navigation when agent issuance is disabled. They assert no
mutation controls and preservation of the original pending command. These are VM
objects and callbacks, not pixels or a simulated successful agent execution.

`tools/hagency/native_management_agent_work.py` inspects an already-open isolated
native window. `--expect hidden` checks old-server absence; `--expect empty` checks
an actual authorized empty index. `--expect record --request-id ... --work-id ...`
requires real records on the first index/history pages and can assert exact
recorded labels with repeated `--expect-text`. It creates no work or fixture data.
The full-window captures require visual inspection and unchanged UX scoring.

Coordinator allocation views and original-command handling (EN/CN):

```sh
python3 apps/hagency/tests/check_allocations.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/allocation-vm/allocations
```

This evaluates the actual shipped subtree and handlers, including distinct request
and allocation revisions, missing server discovery, no second owner approval,
unknown capacity, failed release with retained usage, pagination and uncertain
original commands. It does not establish native pixel or end-to-end reservation
acceptance. `tools/hagency/native_management_allocations.py` is a read-only native
probe for an already-open isolated profile and real allocation; it does not seed
capacity or submit decisions.

## Full bundle root construction

```sh
cargo build --offline --locked --manifest-path tools/hagency-script-review/Cargo.toml
python3 apps/hagency/tests/check_full_bundle.py tools/hagency-script-review/target/debug/hagency-script-review --output .run/hagency-full-bundle-vm
```

This evaluates the entire shipped main.splash using the pinned Makepad VM
(`1f3b1dedfbb81424eb8dbf69e5e2c634fa73dc54`) and inert widget prototypes. Both
English and Chinese must construct exactly heading, navigation, notice and
content at the root. Unlike subtree checks, this detects a missing brace that
lets the streaming parser auto-close at EOF and swallow the root into a function.
It does not render pixels or prove native startup/HTTP success. Retained red
evidence: frozen20 yields zero children; frozen18 and the brace fix yield four.

`--bundle PATH` selects a frozen main.splash for the same reproduction. No source
sections are discarded or replaced. Production callbacks remain unevaluated
until a native view invokes them. Actual native launch remains a separate gate.

## Agent cleanup and original-command recovery

```sh
python3 apps/hagency/tests/check_agent_cleanup.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/agent-cleanup-vm
```

The actual cleanup index/detail and confirmation callbacks are evaluated in
English and Chinese. Checks preserve unknown accounting, separate server/native/
process/Matrix observations, distinguish manager and assigned-admin actions,
retain original IDs after uncertain results, and keep cleanup reachable with
admission disabled. These are VM control checks, not native screenshots.

The read-only `tools/hagency/native_management_agent_cleanup.py` uses an
already-open localhost fixture and real request IDs. Its `hidden`, `empty`, and
`record` modes never cancel or revoke. Record mode requires explicit
`--can-cancel`/`--can-revoke` expectations for any permitted role controls and
can assert actual status labels with repeated `--expect-text` arguments.

## Personal-agent policy review

```sh
python3 apps/hagency/tests/check_personal_settings.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/personal-settings-vm
```

These EN/CN checks evaluate the shipped personal list/detail/editor/review and
original-command callbacks. They cover missing/offline/cached observations,
explicit replacement of existing custom rules, cancellation, exact generation
and revision, safe token bounds, paging and uncertain retry. Run the full bundle
root regression too. Neither test supplies native pixels or a real policy edit.
The corresponding native probe is read-only and requires an existing isolated
personal-agent record (or explicit empty-list expectation).


The personal settings source checks now include the explicit original-key
`personal_agents.observe` request. Refresh remains GET-only. The real HTTP
fixture proves a policy edit followed by observation reports the changed policy,
and that discovery loss after a committed POST keeps the original request pending.
The native probe can inspect an `--expect unobserved` detail and optionally use
`--observation-cancel`; it never sends the observation or saves policy.

## Local task save and recovery UX

`check_operator_task_writers.py` evaluates the full shipped EN/CN source: one Save/Post comment chains native preparation and exact-ID application, uncertain replies keep the saved command, reopening history never applies it automatically, and Delete requires its own confirmation. It also checks optional-field toggles, all existing priority/granularity/label callbacks, and compact list navigation. `check_operator_tasks.py` retains read, paging, assignee, parent and comment checks.

The native `operator_tasks_editor_primary_action_precedes_optional_fields_in_viewport` test checks the real full-bundle Save/Post comment bounds at 1000×800 with the unchanged Splash budget. VM assertions establish behavior, not native pixels or a visual score.

## Task result acknowledgement

```sh
python3 apps/hagency/tests/check_task_acknowledgement.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/task-result-ack-vm
```

These full-bundle handler checks cover twelve sequential saves against a modeled
native eight-unacknowledged-result bound, exact-ID acknowledgement retries,
read-only restart/review, ordinary next edits, and both result navigation and
Details dismissal. Failed or lost acknowledgement preserves the terminal result;
saved, submitting and unknown task changes are never acknowledged automatically.
The model tests script sequencing; actual native SQLite authority and visual
acceptance are separate evidence. Acknowledgement does not delete the task's
canonical command receipt or create another task/comment.

## Live language changes

`check_locale_change.py` checks the shipped `on_app_locale_changed` hook in both
languages. The hook redraws the current page without replaying startup, host
requests, task acknowledgement or pending commands. Native host coverage keeps
the same Splash heap, draft widget and original asynchronous reply callback; the
notification is emitted only when the language changes. Scripts without the
optional hook are left intact. The existing26a real native regression remains
under `.run/management26a-native-i18n-keyboard`: its shell changed to Chinese while
the open app stayed English. New native UI acceptance must verify the paired
fix and original unsaved draft, in addition to these script/host checks.

```sh
python3 apps/hagency/tests/check_locale_change.py \
  tools/hagency-script-review/target/debug/hagency-script-review \
  --output .run/hagency-locale-change
```

## Closed local resource editor

`check_resource_edit.py` runs the shipped callbacks in the pinned VM for EN/CN
entry, qualified model/reasoning selection, unchanged/zero/clear/invalid limits,
current-revision checks, cancellation, exact original-ID recovery and unknown
results. It also evaluates the actual editor subtree with inert widget types;
that is script evidence, not native pixels. The host only accepts the closed
`hagency.resources.edit` proposal and requires a native before/after review.
Native ledger/provider checks and actual hidden-window acceptance remain
separate from these fixture replies.
