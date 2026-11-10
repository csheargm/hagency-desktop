# Hagency Desktop

0.5.0 is prepared but not published: the [release assessment](dev-vibe/release-0.5.0/assessment.zh.md) records outstanding upstream dependency security work. Existing downloads are on [GitHub Releases](https://github.com/hagency-org/hagency-desktop/releases).

English | [简体中文](README.zh-CN.md)

Hagency Desktop is a native Rust/Makepad client for Hagency servers. It combines
Matrix chat, Project workspaces and locally executed AI Agents in one application.
Sign in to a **Hagency server** through its Pasion browser authorization. Ordinary
Matrix servers are not supported by the current login flow.

## What is managed where

- A Project binds one Matrix Space. Its discussion Rooms keep their own membership.
- An Agent is a permanent, server-wide identity owned by its creator. Creation
  assigns the current authorized device; Room bindings are separate.
- The server checks identity, Room admission and the assigned execution device.
  Project/Room administrators manage Agent access and service pauses.
- The owner selects Codex or Claude Code, model, workspace, budgets and tool policy on their own
  device. These resource choices require no server administrator approval.
- One device can run multiple Agents. Switching accounts stops the current owner
  runtime; it does not transfer Agents or merge account data.

## Quick start

1. Enter your Hagency server address and complete Pasion sign-in in the browser.
   The same authorization establishes your Matrix chat and owner-management session.
2. Open **Projects → New Project** (also available from the Projects **⋯** menu).
   Create a Space or select a joined Space. Add discussion Rooms from its details.
3. Open **Agents → New Agent**, enter a name, and wait for the identity and its
   owner private chat to become ready. No Project or Room is required to create it.
4. In **Model & resources**, connect a locally signed-in Codex or Claude Code
   account, then save a model and reasoning effort. Hagency creates a private workspace.
5. Set the Agent, Room and requester policy in **Usage & limits**. Select
   the private chat or a bound Room in **Service** and start responding.
   Actual usage is charged after the task. An admitted task may exceed its budget;
   quota exhaustion does not interrupt its work.

See the [full native quick-start guide](docs/hagency-quickstart.md) for Room
bindings, member invitations, policy boundaries and recovery states.

Project tabs are **Overview / Rooms / Members / Access**. Agent tabs are
**Agent and device / Model & resources / Usage & limits / Service**, for identity/device, local resources, request
policy and quota, and scoped execution/approvals respectively.

## Navigation and retained features

The desktop sidebar has **Projects, Agents, Chats, Contacts and Mini apps**.
Projects, Agents and Chats each have a **⋯** creation menu. **More** opens Discover,
Explore rooms, Moments, File Transfer, Article editor, Browser and Settings.
Project entries have their own **⋯** menu for details, Rooms, members, invitations
and opening the Space. Invitation controls follow actual Matrix permissions.

Matrix chat, contacts, room search and attachments, encrypted human conversations,
Moments, the native article editor and scoped mini-app import/consent/run remain.
The default mini-app catalog has no Palpo app. Hagency management uses the native
Projects and Agents pages; the former Palpo resource/coordinator workflow is retired.
Agents can read encrypted Room history and exchange files through the admitted
Matrix conversation. Shared file publication permits up to 64 files and 64 MiB
per request; hitting that transfer limit returns a tool error and leaves the task running.

Native Codex and Claude sessions retain their context between turns. Shared Claude
on macOS uses a separate workspace and Seatbelt confinement; owner hooks, settings
and unrelated MCP servers are not loaded. Native session files are sealed while
the process is stopped and require fresh scope authorization before reopening.
The runtime requires Claude Code 2.1.322 or newer and Codex 0.167.0 or newer;
older versions are refused. Conversational controls accept authenticated Matrix sender devices
without requiring cross-signing, and still check the card's responder and current
Room membership. They do not grant tool-approval authority.

The renderer remains experimental; arbitrary WeChat HTML import and complete
WeChat compatibility are not claimed. See the [article integration evidence](lab/article-html-integration/README.md).

## Accounts, settings and local data

Use **Settings → Account** and its saved-account picker to add or switch identities.
The bottom-left account menu opens account settings and logout. Each account keeps
its own Matrix device/crypto data and owner profile. Only the selected account syncs.
Switching stops local Agent processing for the previous account; logout stops its
runtime and revokes its current authorizations.

**Settings** contains **Account, Preferences, Privacy and About**. Appearance and
language are in Preferences. Generic external **Agent Access** controls are separate
from Hagency's native Agents; a BotFather or loopback endpoint is not required for
Hagency setup.

`HAGENCY_DESKTOP_DATA_DIR` selects an absolute, isolated data directory. Existing
`RINX_DATA_DIR` and `ROBRIX_DATA_DIR` environment aliases remain in the desktop
launcher. They do not enable compatibility with retired Fleet data. Owner ledgers,
workspaces and provider associations stay scoped to the full authenticated account.
Moving execution to another device does not transfer ownership. This native CLI
cutover requires matching current Desktop, daemon and server builds; it does not
restore previous-release runtime options or approval protocols. Old runtime
intentions are discarded, and each Room must be explicitly started again.

## Build and check

The owner engine is maintained in [`crates/hagency-client`](crates/hagency-client/README.md)
as local Rust libraries. No separate Client checkout or browser UI build is required.
The repository pins Rust 1.98.0.
Install CMake and the platform's native build dependencies, then run:

```sh
cargo run --locked --bin rinx
cargo test --locked --lib
cargo test --locked --manifest-path crates/article-core/Cargo.toml
python3 tools/wechat-ux/check_i18n.py
```

The Cargo package/binary retains the name `rinx`; the application presents itself
as Hagency. Default features use the standalone native application. The retired
`agent_chat` instrumentation is not needed to enable current Project/Agent management.
Mobile packaging requires platform signing and device validation.

Built-in native and OctoScript apps are declared in `system-apps.json`; the article
editor is under `apps/article-editor/`. See the [app development guide](apps/README.md)
and [catalog architecture](docs/adr/0008-rinx-system-app-catalog.md).

## Acknowledgements and license

This repository preserves its Robrix/Robrix2 and Rinx history. Thanks to Kevin Boos,
Project Robius and the contributors to Makepad, Robius, Matrix Rust SDK, Blitz and
Octoscript for the native client foundations.

Distributed under [Apache-2.0](LICENSE). Inherited Robrix code retains its
[MIT copyright and permission notice](LICENSE-MIT). See [NOTICE](NOTICE) and
[third-party notices](licenses/THIRD-PARTY-NOTICES.html).

See [development, isolated profiles and native acceptance](docs/local-development.md).

[GitHub CI and releases](docs/github-ci.md)
