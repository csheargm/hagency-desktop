# One-time setup for the e2e room

The runner does everything it can by itself:

- registers `zz-anna` (alpha), `zz-betty` and `zz-will` (beta) through the
  servers' normal password registration (Pasion native API, the same call the
  desktop's sign-up makes); passwords are generated into
  `hagency-server/secrets/e2e-<name>.password` (0600) and never printed;
- creates "e2e room" on alpha as `zz-anna` (invite power level 0), joins the
  other two personas, and invites Anna (`@csheargm:localhost:8450`) and Betty
  (`@bird:localhost:8452`). The room id is kept in
  `hagency-server/.run/e2e/room.json`.

## What still needs the owners' windows (once)

An Agent serves a room only after its **owner binds it** there. The binding is
an owner command (`agent.bind`) that the desktop runs in-process through its
native owner engine (`/console/api/owned-agents/{id}/bindings`). There is no
external endpoint for it, and the server refuses to serve a room on a bare
puppet invite ("A Matrix puppet invite alone cannot authorize service",
`src/hagency/backend.rs`). So per owner, one action in their own window:

1. **Betty's window** (`Hagency — bird`): click **e2e room** (shows "Invited by
   zz-anna") → **Join**. With "Join rooms I join" on (the default), bird-claude
   and bird-codex follow automatically. If a prompt asks "Bring … into e2e
   room?" or "Allow in group rooms?", click the affirmative button.
2. **Anna's window** (`Hagency — csheargm`): same for **e2e room**, so
   anna-claude and yue-claude follow.

Check: `python3 run.py --only 1` stops reporting `BLOCKED (setup)`, and the
report's "Setup state" shows `agent:<name>: join` for all four.

Why the runner does not do this itself:

- Betty's demo password file (`secrets/demo-betty-password`) does not sign in
  `@bird` (`invalid_credentials`), and Anna uses GitHub sign-in only, so the
  runner has no owner session. Even with one, joining over the API would not
  bind anything: the follow-on-join logic runs only on the desktop's own
  "Joined" UI action.
- Driving the windows with desktop automation was attempted on 2026-11-17 and
  stopped (the room list is a Makepad canvas that needs foreground clicks).

## Removing the gap (proposed)

Cheapest route, no product change: every Makepad app has a localhost control
bridge (`MAKEPAD_REMOTE=<port>`, used by `tools/agentchat-probe/probe_approval.py`:
`/snap?q=<text>` lists widgets, `/click?x=&y=&wait=1` clicks with real
hit-testing). The demo windows are launched by `int-demo-up.sh` without it
(checked 2026-11-17: neither window listens on a bridge port). Adding
`--env MAKEPAD_REMOTE=<port>` per window there (next demo restart) lets the
runner click **e2e room → Join** itself. Not done here: it needs a window
restart, which would interrupt the live demo.

Alternatively a test-only owner hook, for example a
`HAGENCY_DEMO_AUTOMATION=1`-gated local socket in the desktop that accepts
`agent.bind {agentId, roomId}` from the same machine user, or a
"auto-accept invites from <prefix>" demo flag. Either is a product change and
belongs in its own session (R8).
