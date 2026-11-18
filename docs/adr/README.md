# Architecture decision records

Records distinguish proposed designs from decisions that have been accepted and
implemented. An ADR is not evidence that a feature has shipped.

| Record | Status | Subject |
| --- | --- | --- |
| [0001](0001-matrix-moments.md) | Accepted; first slice implemented | Matrix Moments, timeline audiences, and private self-chat/File Transfer |
| [0002](0002-octoscript-mini-app-authority.md) | Proposed; source review complete | Octoscript mini-app containment, account delegation and Markdown/HTML editor |
| [0003](0003-shared-article-components.md) | Accepted; implemented | Standalone article libraries, host authority and optional Blitz preview |
| [0006](0006-shared-app-hub-miniapps.md) | Accepted; implemented | Shared App Hub distribution and native mini-app library |
| [0007](0007-host-owned-octos-app-peers.md) | Accepted; implemented in part (native hosted flows unverified) | Host-owned Octos app peers and Rinx deployment modes |
| [0008](0008-rinx-system-app-catalog.md) | Accepted; implemented | Bundled native/OctoScript catalog, packaging and deployment boundaries |
| [0009](0009-shared-reloadable-themes.md) | Accepted; Rinx implementation delivered; host/device gates open | Shared OctoSense/Makepad themes, Rinx and mini-app design language, live reapply and customer themes |
| [0010](0010-palpo-admin-miniapp.md) | Implemented foundation; partly superseded by 0011 | Shared Matrix login, mini-app host, Inbox and notifications; historical workflow review |
| [0011](0011-hagency-server-engagements.md) | Accepted; implementation pending | Rust workflow migration, multiple Hagency server engagements, coordinator approvals, bounded resource allocations and agent visibility |
| [0012](0012-owner-daemon-shared-dashboard.md) | Implemented; runtime and numerical UX gates passed; manual VoiceOver open; standalone app removed by Client ADR-192 | Independent owner daemon, shared standalone/embedded dashboard, coding-plan telemetry and UX above 95/100 |
| [0016](0016-distributed-github-loop.md) | Proposed; first milestone implemented | Distributed GitHub loop: owner work list (developer/reviewer/maintainer), GitHub claims, owner-side maintainer merge rules |
| [0018](0018-ollama-runtime-caps.md) | Accepted | Ollama server endpoints are http only; usage caps live server-side for hosted Agents |
| [0019](0019-agents-know-and-share-watches.md) | Accepted | Agents know their own watches; room_watches shares only this room's watches |
| [0020](0020-issue-loop-tracker.md) | Accepted; implemented | Issue loop tracker: per-issue state, retries with backoff, PR follow-through to review and merge, restart-safe |
| [0021](0021-owner-approvals-inbox.md) | Accepted | Owner approvals inbox: every pending approval of your agents is shown, notified, and recorded when it expires |
| [0022](0022-push-freely-covers-fix-steps.md) | Accepted | "Push freely" also covers the local fix steps (status/diff/log/add/commit/restore, pull, clone, reads and edits) in the watched repo's own checkout |
| [0023](0023-room-repo-status.md) | Accepted | Room repository status from every owner's tracker (room state), Agent tasks and /status for every room, linked repos, card-free reads over the room's repository set |
| [0024](0024-agent-replies-in-the-room.md) | Accepted | Agent answers are main-timeline replies quoting the request (room setting "Agent replies: In the room / In threads"); progress opens in place |
