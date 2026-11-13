# Personal-agent settings checkpoint

This ports a bounded part of the original owner settings flow into Hagency's mini app. It is a source checkpoint; native pixel inspection and a real execution-device edit have not been accepted.

## Implemented source

The Overview entry opens **Personal agents**. The list and detail reuse the original owner `Command::Agents` and `AgentConfigurationStatus` APIs; server filtering keeps shared project identities out. Detail exposes the observed policy revision, token budget/period, request and high-risk-tool modes, rule counts, personal consumed/held amounts, and safe model/reasoning/search/workspace-presence metadata. Cached, offline, unsupported and missing observations are distinct. Unknown observations do not become zero usage or a running agent.

The explicit **Read current settings** action reviews the assigned device, then submits the original environment observation request with a durable native command ID. This fills an initially missing observation and refreshes one invalidated by a saved policy change. Plain **Refresh** only reads the server’s saved observation. The returned native receipt confirms the original read completed; the separate displayed observation timestamp determines freshness. Provider credentials and workspace paths remain private.

The policy editor supports the original budget modes (unset, unlimited, token limit), lifetime/UTC day/UTC month periods, allow/deny/ask-owner requests, and deny/ask-owner/full-trust high-risk tools. Review precedes Save. Existing custom rules are never silently replaced: their counts and replacement warning remain visible, and an explicit supported replacement mode is required. Cancel leaves the original policy unchanged.

**Full trust** lets a local Agent use every tool without asking its owner, as a pooled Agent's full trust does. A Codex turn then starts with approval policy `never` and no sandbox (files outside the work folder and the network included); Claude Code's tool requests are allowed without a card. Each turn states its sandbox and approval policy again, so the next turn after the owner takes the trust back is confined. It is the owner's choice per Agent and is shown in the warning colour with the same warning in the editor and on the review step. On the Agent layer it is the owner's trust in their own messages: anyone else who calls the Agent gets it only in a Room where the owner also saved full trust on that Room and on that member.

Full trust is carried as one tool rule the server already relays, `AllowWithRules { tools: ["*"], directories: ["/"] }`, so no server change is needed; a program that matches rules exactly finds no such tool and allows nothing. Only that exact pair is full trust. It can be saved only through its own choice: the exact-rule editors refuse the pair. For the owner's own messages, full trust on the Agent layer also covers a Room or member layer the owner never saved (which otherwise denies by default); a Room or member layer with a saved setting, a reset one included, keeps that setting. Anyone else's messages meet every layer, so an unsaved Room or member layer still denies them: the ledger compares the dispatch's requester with the ledger's owner, both for a Codex turn's unconfined start (`fully_trusts`) and for each tool request (`validate_tool`). Full trust on a Room or member layer alone does not lift the Agent layer's own rule. A native request for wider permissions (`permission_expansion`) still goes to the owner.

The Agent's web search switch (model settings) now applies to Claude Code as well as Codex: when it is off a local Claude Agent starts with `WebSearch` and `WebFetch` disallowed, as a pooled Claude Agent does.

The closed `hagency.personal_agents.policy.save` service wraps the existing `AgentConfiguration` / `SaveAgentPolicy` execution-device command. It preserves the caller's exact command ID, agent generation, original policy revision and native-only requester origin/issuer/subject/owner/device/generation. A network discovery await is followed by a fresh SDK/device check before sending. The executor performs the policy revision compare-and-swap; conflict requires a fresh observation and review instead of overwriting newer settings.

The native intent is saved before an effect. Interrupted or unauthorized replies retain the original pending command; retry uses its original input and idempotency key. A known first-attempt refusal can be acknowledged. The same refusal after an earlier uncertain attempt remains unknown. Capability loss (`configuration_unsupported`) always retains custody because it can occur during polling after the original POST committed. Returned success is labelled an original historical policy-change result, independently from the latest device observation. A replacement requester cannot issue an uncertain command under a new identity.

## Exact surface and authority

| Service | Authority | Effect |
|---|---|---|
| `hagency.personal_agents.list` | ReadPersonalAgentSettings, current owner/SDK | Bounded 16-row personal-only page |
| `hagency.personal_agents.get` | ReadPersonalAgentSettings, current owner/SDK | Safe existing device observation; no environment-request POST |
| `hagency.personal_agents.observe` | ReadPersonalAgentSettings, RequestPersonalAgentSettings, ReadIntents, exact current SDK/device | Explicit original-key device observation; no policy mutation |
| `hagency.personal_agents.policy.save` | ReadPersonalAgentSettings, EditPersonalAgentPolicy, ReadIntents, current exact SDK/device | Original revision-checked remote policy command |

Existing intent retry/acknowledge services retain the operation's original grants and journal rules. No generic command, URL, filesystem path, workspace root, provider credential or custom rule text crosses this surface. It does not bypass an adopted personal-database fence: local execution remains in the existing authorized execution-device service.

## Remaining original functionality

| Feature family | Current checkpoint | Still missing / unverified |
|---|---|---|
| Personal policy and budget | Read/edit source implemented | Actual signed-in execution-device edit, restart recovery and EN/CN whole-window acceptance |
| Model/reasoning/search/workspace | Read-only safe observation | Trusted model selection and workspace mutation UI |
| Custom tool/directory rules | Counts plus explicit reviewed replacement | Rule editor preserving and validating native-only path scope |
| Execution grants / tool proposals / YOLO | Not ported by this change | Original proposal, grant/revoke and workspace inspection flows |
| Shared project-agent settings | Unchanged | Versioned central policy amendment; personal settings never authorize shared agents |
| Shared token top-ups | Unchanged | Assigned-project-admin delta approval, native append-only accounting and same-period conservation |

This does not close the 104-operation parity inventory. Personal policy amounts are not project capacity or a provider quota.

## Reproducible qualification

- [Native read projection](../../../hagency-client/native/hagency/src/console/native/personal_agent_settings.rs) and [policy writer](../../../hagency-client/native/hagency/src/console/native/personal_agent_policy.rs) and [explicit observation](../../../hagency-client/native/hagency/src/console/native/personal_agent_observation.rs).
- [Real loopback HTTP tests](../../../hagency-client/native/hagency/src/console/native/personal_agent_settings_http_tests.rs): wrong permission/original requester causes zero policy POSTs; lost reply and post-commit SDK change retain one effect; original retry, revision conflict and privacy checks; applied POST followed by discovery loss cannot become an acknowledged failure; explicit missing/post-save observation and historical-generation recovery use one original request.
- [Closed host adapter](../../src/host/hagency/personal.rs) rejects unknown command/path/credential fields and unsupported policy modes.
- [Pinned VM tests](../../apps/hagency/tests/check_personal_settings.py) evaluate actual EN/CN list/detail/edit/review children and callbacks. They are not pixel captures. The full shipped-root regression is also required.
- [Read-only native probe](../../tools/hagency/native_management_personal_settings.py) targets an operator-selected existing personal agent or an actually empty list. It does not save a policy, create an agent or alter the signed-in account.
