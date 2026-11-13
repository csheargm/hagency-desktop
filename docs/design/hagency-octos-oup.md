# Octos resources over OUP

Hagency can use an owner-selected local Octos profile as a pooled Agent. The
owner can create a separate pool for each configured primary or fallback model,
including models on local and custom endpoints. The first pool sets the
profile's monthly total; all its model pools share that total, while a pool can
have a lower individual maximum. Different profiles retain separate budgets.
This is an owner declaration, not a verified provider account or balance. The
owner's confirmation saves an unpublished resource; connecting and publishing
it are separate operations.

The current management UI cannot amend that profile-wide total after the first
pool is created. Pool-specific limits can still be amended within it.

Only models actually configured in the profile are offered; a general Octos
catalog entry does not establish a usable provider route or credentials. A
fallback whose model ID occurs on more than one route is omitted because the
pool currently identifies a selection by model ID. The inventory and ledger
are bounded to 64 model choices and 64 recorded model pools per profile. Each process gets a
private snapshot with its approved model as the primary and no automatic
fallback; the original profile is never rewritten, and another model cannot
silently consume this pool's budget. Changing the selected model's route or
credentials requires renewed owner confirmation. Editing the original profile
also stops existing guarded processes, even when the edit only adds a model.

The owner device starts a fresh scoped Octos process per admitted call. OUP
provides streamed responses, multiple-choice and free-form questions and answers, active-turn steering,
interrupts, tool calls, and usage. The room history and encrypted Matrix I/O
remain under Hagency's existing room and device authorization. Successor
requests retain the original resource, conversation scope, policy, and budget.
The server advertises `shared-octos-execution-v1` and routes these commands
only to v5 workers.

While a call is active, `hagency_report_progress` lets Octos write a separate
message to its originating room. The host also forwards completed intermediate
assistant segments and correlated OUP background completions. Each message
is recorded locally under the originating call and native message identity;
the sender rechecks room authority and retries with the same Matrix transaction.
The outbox is encrypted locally and never asks the model to reproduce a lost
message. The tool reports that the message was queued, not that delivery has
already succeeded. Octos upstream's proposed `event.agent_report.v1` is a
different, not-yet-merged protocol and is not negotiated here.

This delivery is tied to an admitted active run, not an idle agent session.
Pending messages cannot be sent after the originating room authority has been
permanently revoked. These limits need to be addressed before claiming complete
autonomous-message delivery.

The shared runtime currently requires macOS and an Octos binary that supports
`serve --stdio --host-scoped-profile` and advertises the host-scoped OUP
capabilities. Hagency looks for `octos` in the connected user's normal binary
locations; `HAGENCY_OCTOS_BINARY` can select the built binary for a development
instance. The server schema migration and desktop/Octos changes must all be
installed before a live shared Agent can execute. An older Octos binary fails
the scoped capability check rather than running with weaker isolation.

The selected profile and its inherited defaults are copied to a private,
temporary process scope. Login-store, Keychain and explicitly selected
environment credentials are resolved locally; other login-store providers and
refresh tokens are not copied. An active login-store credential changing
invalidates the process guard. No credential is included in a pool record or
sent to the Hagency server.

An Octos turn with missing usage remains unreported. A nonempty session ledger
alone cannot prove that it includes that turn, so the call remains unknown
for reconciliation. Postpaid calls hold no estimated budget; existing
prepaid reservations remain held. A failed or interrupted turn with an exact
usage receipt is settled using that receipt.

The shared coding policy exposes confined text read, write, edit, directory
listing, glob and content search. Search does not follow symbolic links and is
bounded. In the current scoped Octos build, grep honors workspace `.ignore`,
`.gitignore`, and in-workspace `.git/info/exclude` rules, including nested
overrides; glob follows the personal CLI's behavior and does not apply those
rules. Global Git ignores and ignore files outside the workspace are not read.
Ignore file size and rule counts are bounded. A `.git` file identifying a
worktree is recognized, but its external Git directory is not followed.
The scoped Octos binary must advertise `runtime.host_scoped_search.v1` before
Hagency enables those search tools; that capability guarantees confinement,
not the newer ignore behavior of this build. The agent can also publish plan
updates to the run's live preview. When Octos advertises `runtime.host_scoped_vision.v1`,
the agent can view workspace PNG, JPEG, GIF and WEBP images up to 5 MiB; image
bytes are snapshotted before being sent to the model. When Octos advertises `runtime.host_patch_worker.v1`,
the agent can apply a Codex patch envelope or a single-file unified diff via a
profile-free worker in a separately supervised workspace-only sandbox. Patch
requests are limited to 64 KiB, and cancelling the call stops the worker.

Shell commands run under a separately supervised sandbox. Standard policy asks
the resource owner before every command; automatic approval still uses the
same workspace boundary. A command may use a PTY or pipe, and the agent can
send input and collect output while that command runs. At most four commands
may run at once, and they stop when the activity is revoked or the turn ends.
The Apple Command Line Tools and macOS SDK are available read-only to
commands, including interactive commands, when installed under root-owned
system directories without group or other write mode bits that the host itself
cannot write. Commands have no network access. When enabled, web fetch rejects
private addresses and web search cannot use the profile's private SearXNG
endpoint.

This is not yet the complete personal CLI surface. Hooks, plugins, MCP, native
background tasks and subagents remain unavailable in the shared process.
Terminal sessions are model-driven; there is no direct
human terminal pane. The scoped shell cannot access the owner's custom
toolchains or network. Adding these capabilities requires
their processes, credentials, approvals, budgets and cancellation to obey the
same room boundary. In particular, a child agent must inherit the host-approved
tool roster and the selected model/profile, stop with its originating activity,
and account for all child provider calls. A failed or interrupted child may
have usage that cannot be established; the host must retain its reservation
as unknown instead of settling an incomplete count as the total. Octos's
current native child registry and failure-usage path do not yet meet those
requirements, so the shared policy does not enable native spawn.

The offline integration tests use the real scoped Octos executable and a
synthetic loopback model. The shared-room fixture covers questions, replies,
owner decisions and settlement; a separate test verifies Olm-encrypted
approval messages. These do not replace a live encrypted Matrix-room test.
