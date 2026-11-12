[English](hagency-quickstart.md) | [简体中文](hagency-quickstart.zh-CN.md)

# Native Hagency quick start

This guide describes the current Projects/Agents pages. The Palpo mini-app,
Fleet/engagement/resource requests and coordinator approvals are retired.

## Sign in once

Enter a Hagency server address on the login page, then finish Pasion sign-in and
consent in your browser. The server must identify itself as a supported Hagency
service; a normal Matrix server is rejected. The Matrix SDK owns this login's
refresh and persistence. Native management uses the same current account proof,
without asking for a copied token or a second Matrix login.

When Pasion advertises account creation, the selected server also shows **Create
an account**. This opens Pasion registration in your browser with OAuth
`prompt=create`; after registration and consent, the same SDK callback signs you
in to Desktop. A server that disables registration hides this option.

Saved accounts are available on the login page and under **Settings → Account**.
Switching accounts stops the current owner's local Agent runtime and clears its
active permissions. The previous account's separate data remains; Agent ownership
never transfers with a switch.

## Create a space or room

Open the conversation list’s **+ → Create a space or room…** and follow the
three-step wizard. The same **+** menu also offers **Article editor**, which
opens its mini-app tab and retains the current editing session.

| Choice | Purpose | Next step |
| --- | --- | --- |
| Ordinary space | Organize people and conversations without Agents | Create rooms and invite members |
| Agent space | Collaborate with people and Agents; Hagency connects the Project automatically | Create rooms, invite Agent owners and members, then add Agents |
| Chat room | Exchange messages inside a selected space | Choose its space; Agent-space rooms are connected to the Project automatically |

Spaces organize rooms; each room has its own members and encryption settings.
Joining a space does not automatically join its rooms. The wizard identifies the
space type and required permissions. Refresh if its type cannot be checked. If
creation succeeds but connection is incomplete, use **Finish setup** in the same
wizard to complete that creation.

In an Agent-space room, open **Chat information → Members → Add Agent** to select
an existing local or pooled Agent. Its owner must join both the space and room.
Both Agent types follow the same reply rule: the owner can chat without @ in the
original one-to-one room. Once other members join, every request must @ the Agent,
including the owner’s requests and thread replies. The original room is retained;
the same Agent can also join additional rooms in Agent spaces.

Project/Room access policy controls who may connect an Agent to that scope. Deny
entries take priority. It does not approve token allocations. **Service pause**
is separate and can stop current service; clearing it does not automatically
resume the owner's paused bindings. Ordinary members cannot write these settings.

## Create a global Agent

Open **Agents → New Agent**, enter its display name and create it. This allocates
a permanent owner-bound puppet identity and assigns the current authorized device.
It does not allocate a Project or discussion Room. Wait for provisioning to finish.
The owner-private-chat setup preserves its original creation request on retry.
Use **Set up / retry private chat** if needed, then **Open private chat** when ready.

Only the assigned device may execute this Agent. **Handle on this device** changes
that assignment to the current owner's device with an explicit confirmation and
revision check. It does not transfer ownership, copy budgets, reset spent tokens
or authorize repeating unknown work. Historical ledger recovery may still be
required before this device can run a new model request.

Add discussion bindings with **Add Room binding**, or open a discussion Room's
chat information, **Members → Invite my Agent**, and select an existing Agent.
The backend still checks actual Project/Room admission, membership and puppet
invitation. This picker does not create a second Agent identity.

## Configure the four Agent detail tabs

| Tab | Configure or inspect |
| --- | --- |
| Agent | Identity/device, owner private chat, Room bindings, pause/resume, leave and permanent retirement. |
| Model & resources | Provider account, model, reasoning options and saved Agent workspace on this device. |
| Limits | Agent total, selected Room total, and selected requester in that Room. |
| Service | Selected Room runtime, scoped files and pending approvals. Actual usage is charged after each task. |

Select Codex, Claude Code or Octos in **Local agent runtime**.
Choose **Connect my local account** to reuse the current machine's
Codex login. No second login is needed when that account is already valid.
Disconnecting its association keeps your everyday Codex signed in. Separate
Codex sign-in is also available; cancel/open-URL/sign-out controls apply to that
separate account. Save a model and reasoning effort from the real catalog. A private workspace is
created automatically; a custom workspace is optional under Advanced settings.

Codex's **Live web search** is enabled by default and can be disabled in Model & resources.
The setting belongs to this Agent and applies across its Rooms. Save, then stop and restart
the service to apply changes. Live search retrieves current sources; local command networking
remains separately controlled.

The model/workspace belong to this Agent across its Rooms. Conversations, budgets
and approvals stay Room-scoped. The server administrator does not approve these
local resource choices.

Agent details select the owner’s private chat by default. Service shows the current chat directly; switching to another Room and managing its binding are under **Advanced Room settings**. Opening Service from a Project Room retains that exact Room.

In **Limits**, choose the entire Agent, one Room, or one member in a Room.
Missing limits allow requests by default; configured token limits apply together.
Each limit supports cumulative, UTC day, or UTC month periods. Zero tokens blocks
usage in that scope. Existing Unset values also mean no token limit.
Request handling can Allow, Deny or Ask owner. Tool policy can deny high risk, ask
per exact tool, or restrict exact tools/directories. Saving preserves spent usage
and unresolved holds. A budget hold is not measured usage of zero.

Claude Code uses its existing local sign-in and the Sonnet, Opus or Haiku model
alias with supported effort choices. Octos uses an existing local profile; profiles
with the same model remain distinct and keep their own reasoning settings.
Credentials remain in the provider app. Disconnecting removes this owner's
association without signing out that app. Stop the service before changing
runtimes, save the profile, then restart.

Claude/Octos native tools use the existing Agent, Room and requester permission
layers and owner approval list. A denial at any layer blocks the operation. The
Matrix file bridge is available to Codex and Claude for the admitted Room.

## Start and chat

In **Service**, select an active private chat or Room binding. Wait for
the saved local runtime profile and account check to finish, and save a supported
reasoning effort in Model & resources. Accounting is postpaid: admitted work may
exceed its budget and continues until completion or an explicit stop. Uncertain
usage does not discard a completed reply and is not invented or used to replay
the model. Codex and Claude expose native tools subject to the configured approval
policy and host isolation. Shared Matrix file sends are bounded to 64 files and
64 MiB per request; an exact retry retains its original delivery receipt.

**Start responding** starts only that selected scope. **Stop responding** stops it; another
running Room can stay active. Your owner private chat does not need mentions;
ordinary top-level DM turns share their Room conversation. Explicit threads remain
separate. In a collaboration Room, every message requires a real Matrix Agent @mention by default, including thread follow-ups.
Under Agent details → Advanced Room settings, select a Room and enable automatic thread follow-ups in **Room reply behavior**.
After an @mention activates a thread, any member can follow up there without another mention. New threads still require @.
The setting applies only to that Agent in that Room. A started model turn can receive the puppet's 👀 processing reaction.

Pending request/tool approvals appear in the Service tab for the selected Room.
While Service is open, approvals refresh automatically with an in-app notice for
new requests, exact action details and an expiry countdown. Failed refreshes disable
decisions. A submitted decision is not proof of successful execution; check the
Room reply and runtime status. An uncertain submission must not be resent.
Approval is for the exact
request or tool shown. Agent encrypted Room execution uses the Agent's current
device and Room authorization for history, questions and files.

### Restart recovery

Restart recovery restores current-format runtime intentions only for Room scopes
you explicitly started and have not explicitly stopped. It uses the saved effort
and host-files choice after fresh identity, device, lease and resource checks.
**Stop responding excludes that scope from restart recovery**; a normal shutdown
does not. Old-format intentions are discarded at this cutover, requiring an
explicit new start. A saved model alone does not start every Room. Missing
resources, revoked membership or another assigned device block recovery; there
is no model replay or automatic takeover.

Agent Unlimited does not bypass finite Room/member limits or explicit request denial. A sent paused-policy reply is not successful model inference. See the [runtime recovery record](https://github.com/hagency-org/hagency-client/blob/6cd99ceb84ae407b2315dcd0f5e900022eebac06/docs/design/2026-10-08-agent-runtime-recovery.zh-CN.md) for ordinary/quick restart, explicit Stop exclusion, and remaining untested branches.

## Failure and recovery states

| State | Action |
| --- | --- |
| Loading/unavailable | Refresh or retry after checking the connection; late results from a previous account/view cannot replace the current one. |
| Agent creating or private chat pending | Wait; retry the same saved request instead of creating another identity/Room. |
| Other execution device | Use that device, or explicitly assign this device and satisfy ledger recovery checks. |
| Missing model/workspace/account | Connect the account and save a model in Model & resources; the private workspace is automatic. |
| Budget exhausted/denied/approval pending | Configure the matching layer or review the exact pending request. |
| Unknown cost or side effects | Keep the available reply and inspect the recorded outcome; never repeat the model solely to recover its usage. |
| Puppet/owner membership lost | Restore real permissions, wait for confirmed departure, then explicitly rebind/re-adopt. Old dispatches stay fenced. |
| Ordinary service pause | Clear the applicable pause and explicitly resume when permitted. |
| Encrypted Agent Room awaiting keys | Check that the Agent's current device is joined and can decrypt the Room. |

## Other retained pages

**Chats** includes search, attachments, Room information, members and invitations.
**Contacts** opens ordinary conversations. **Mini apps** keeps non-Palpo built-ins,
App Hub, local import, permission review and scoped execution. In the left bar,
**File Transfer** opens your private File Transfer chat (on mobile it is under
Discover). On desktop, the **+** (New or Join) to the right of the chat list's
search field holds New workspace, New room or space, and Join or explore rooms;
on mobile, Explore Groups & Spaces is under Discover. **More** opens Discover,
Explore rooms, Moments, Article editor, Browser and Settings.

Settings has Account, Preferences, Privacy and About. Generic external Agent
Access/BotFather controls are separate tools, not part of native Hagency Agent
setup. Some management text is still English when the interface language is Chinese.

See [development and local diagnostics](local-development.md) for building and connection checks.
