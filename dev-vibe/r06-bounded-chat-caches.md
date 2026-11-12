# R06: bounded chat cache residency

This PR adapts the Client memory work (`6e7a84d9`, `8bf7504a`) to Desktop's official SDK and current Makepad. Desktop PR 53 remains separate: its font/redraw improvements do not govern these application-owned caches. This PR stacks on R05/PR 58 and does not upgrade or replace the SDK/framework.

## Resource ownership

- One weak allocation ledger admits at most 64 MiB / 512 encoded image allocations, with an 8 MiB per-image limit. Shared Arcs count once and stay charged while a callback, queue, model or viewer still holds them. In-flight bounded HTTP buffers are a separate transient working set.
- Media LRU: 32 MiB / 128 entries per timeline, together with the shared ledger. Avatars: 16 MiB / 256 entries, a 256-result completion queue and 64 completions per UI turn. Pending entries are protected from duplicate requests. Room/space-avatar and full-viewer images pass through the same encoded admission.
- Raster/animation headers are checked before decode: 16 MiB final pixel/atlas estimate, with a shared 128 MiB decode/composition/upload working estimate. Reservations follow pending original jobs and live widget/viewer readers. Closing a viewer cannot release a still-running decoder's reservation. Hidden/recycled rows detach image leases; stale draw areas retire pooled readers. Native texture-cache geometry is measured conservatively against 256 MiB; this is not an OS VRAM measurement or a hard process RSS cap.
- Completed decodes are handled at the application entry point even if their view disappeared. Account/SDK-specific cache namespaces prevent reuse across sessions; retired decoded plaintext is discarded before entering a later account's cache. Viewer fetch actions also check their original session and MXC identity.

## Timeline lifecycle

Room sync retains room metadata but does not eagerly construct one SDK timeline per joined room. Creation is serialized, with at most 16 retained main/thread timelines; visible windows are never chosen for eviction. A room hides by saving its lightweight state, then dropping the backend subscriber/timeline and its typing/pin/identity subscriptions. Cancelled opens are fenced by live UI interest before/after creation. Auxiliary subscriptions retain abort receipts on their exact SDK controller, so hiding does not wait behind the actor request queue. Abandoned unclaimed opens expire after 30 seconds, collected at most once per 30 seconds on UI timers. Read-marker operations use at most four temporary contexts, not permanent hidden timelines.

The SDK's existing last-subscriber auto-shrink can then release loaded history chunks while preserving its encrypted SQLite history. The application clears parked event vectors, encoded/link caches, members and draw metadata through a global action hook. It preserves the input-bar draft/reply/edit/upload state, pending download outcomes and a stable event-ID reading anchor. Reopening reloads history and restores that anchor through the existing history-jump flow.

Timeline channels coalesce replaceable snapshots without crossing ordered control events. Evicted endpoints reject late positional snapshots while forwarding mutation/upload/download results to a replacement context. Delayed eviction actions cannot clear a newer UI endpoint. Authoritative send queues, crypto stores, attachment descriptors and user-saved files are not cache-eviction targets.

## Limits and validation

These budgets cover application-owned encoded images, admitted image work and retained timeline controllers. A user's actively paginated timeline, SDK room metadata/SQLite internals, text/layout storage, fonts, swapchains and native driver retirement are not a hard total-byte/process cap. This PR does not import the Client fork's additional SDK history-window APIs or its separate encrypted history-search product.

Tests include actual official SDK sync for 40 rooms, zero eagerly created controllers, visible-window saturation, hidden-window reclamation, actual original SDK-owner release (including auxiliary tasks), cancelled-open suppression and late-result delivery after reopen. A real RoomScreen fixture verifies draft/reading-anchor preservation and rejection of stale eviction notifications. Allocation/queue tests cover live Arc accounting, animation/icon headers, pending decode ownership, actual pool cancellation, orphaned/retired completions, avatar/media limits and 20,000 endpoint replacements. Results and required release checks are recorded in the PR description. Windows/Linux native UI and total RSS/VRAM are not certified by headless macOS fixtures.
