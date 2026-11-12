# Local task writers

This checkpoint adds the retained local task operations to the built-in Hagency
mini app. It does not dispatch model work, change shared-agent assignments, or
write Matrix project records. The local board and Matrix Projects remain
separate views.

The writer uses the current canonical profile and its initialization proof.
An absent or invalid proof denies task writes. Each operation still checks the
current package, SDK identity and task-specific permission.

## Closed services and custody

- `hagency.tasks.create`: title, optional description/priority/granularity,
  optional assignee/parentId, labels.
- `hagency.tasks.edit`: id, expectedRevision, explicitly supplied replacement
  fields. Omitted fields remain unchanged; nullable fields can be cleared.
- `hagency.tasks.delete` and `.accept`: id, expectedRevision.
- `hagency.tasks.transition`: id, expectedRevision, status, and required
  waitingReason/waitingUntil when entering blocked.
- `hagency.tasks.comment`: id, expectedRevision, text. The author comes from the
  currently authenticated profile.

These six services **prepare only**. Native code generates and durably saves the
original command ID, and a task ID for Create, before returning the review.
`hagency.tasks.command.get {commandId}` reads that original review.
`hagency.tasks.command.apply {commandId}` applies or recovers it. Script cannot
supply an author, profile, creation command ID, time, path, or raw operation.
Each operation has its own native permission; read/recovery does not acquire
missing mutation permission. All calls remain subject to current package and
SDK authorization before and after awaits.

The canonical transaction binds operation, full profile, authenticated author,
original payload and expected revision to its immutable receipt. Receipt lookup
precedes task existence and CAS checks. Lost replies, retirement after commit,
and restart therefore recover the original task/comment result. Revision
conflict stops the request; it never updates the expected revision automatically.

The UI keeps editing, saving, saved, submitting, unknown, failed and recorded
states separate. Closing a saved review does not apply it. Unknown preparation
requires inspecting saved requests rather than minting another identity. A
recorded result is historical; Refresh reads current task state.

## Original behavior retained

Created → Accepted → In progress; In progress → Blocked or Done; Blocked →
In progress. Accept is available only for Created. The first start timestamp is
retained, blocked metadata is cleared on continuation, and Done is terminal.
Deletion is permitted for any status, removes comments, and retains child tasks'
parent references. Operator edits are not restricted to the recorded creator or
assignee. Existing authors are descriptive historical strings; new comments use
the current account. Original text/label normalization and limits are retained.

## Reproducible source checks

Use the tracked `tools/hagency-script-review` helper pinned to Makepad
`1f3b1dedfbb81424eb8dbf69e5e2c634fa73dc54`:

```
python3 apps/hagency/tests/check_full_bundle.py PATH_TO_HELPER --output OUT/root
python3 apps/hagency/tests/check_operator_tasks.py PATH_TO_HELPER --output OUT/read
python3 apps/hagency/tests/check_operator_task_writers.py PATH_TO_HELPER --output OUT/write
```

These execute the full shipped source and selected real callback subtrees, not
pixels. The Rust `operator_tasks_` filter also constructs/draws actual widgets:
all five board counters, the original zero-width negative control, distinct task
rows and status controls, and initial Create/empty/filter viewport bounds.
The `task_writer_host_` filter checks closed payloads and original-only Apply.
Native `native_task_writer_` uses isolated SDK/HTTP and canonical storage for
restart/lost-reply/retirement/permission recovery. Store `task_writer_` covers
atomic receipts, CAS, rollback, comments, deletion and queued authority loss.

Actual account UI writes and visual acceptance remain root-owned and pending.
No live fixture, server policy or original artifact is changed by these tests.
