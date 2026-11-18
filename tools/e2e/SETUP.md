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

## Owner steps: automated through each window's bridge

An Agent serves a room only after its **owner binds it** there, and that is an
owner command the desktop runs in-process (no external endpoint; the server
ignores a bare puppet invite). The runner therefore drives the owners' own
windows through Makepad's localhost bridge (`MAKEPAD_REMOTE`, enabled in
`int-demo-up.sh`: Anna 127.0.0.1:8771, Betty 127.0.0.1:8772).

`run.py` preflight, per owner, skipped when the owner and all their Agents are
already joined (checked over Matrix as zz-anna):

1. open **e2e room** in the room list → click **Join Room** (`accept_button`),
   then wait until Matrix shows the owner joined;
2. answer only prompts whose sentence names the e2e room: **Bring**
   (`bring`) / **Allow in group rooms**;
3. for each Agent still absent: **+** (`new_or_join_button`) → **Add agent to
   this room** (`plus_add_agent`) → the Agent's row → **Invite**
   (`confirm_button`) → **Allow in group rooms** if the private-Agent prompt
   appears; then wait until Matrix shows the Agent joined.

If a port is closed the report says "bridge not up yet" and the agent
scenarios stay BLOCKED. A missing button stops that owner's setup with a named
`StepNotFound` (never a guess). `--no-ui-setup` skips all of this.

Other bridge uses, all scoped to what the runner caused:
- scenario 6 **denies** (never approves) the permission card whose own text
  (`title`/`context`/`details`) names the runner's unique branch;
- scenario 7, on failure, opens the dev room in Betty's window and copies the
  visible lines that mention the test issues (read only, sends nothing).

The bridge's `/gq`, `/quit` and `/close` routes are refused by the client.

## History: before the bridge



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

The bridge route above replaced the manual clicks (2026-11-17).
