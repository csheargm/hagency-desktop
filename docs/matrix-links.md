# Matrix link navigation

Hagency Desktop opens `https://matrix.to` and `matrix:` links to rooms, room
aliases and events inside the signed-in account. Links clicked in conversations,
Space topics and common app URL actions use the same resolver. Native link events
already delivered by the host also use it; this change does not register new OS
protocol associations.

- A joined Room opens directly. Room aliases resolve to their actual Room ID.
- A Room or Space you have not joined shows its preview and a confirmation.
  Nothing joins until you accept. Routing servers from the link are retained.
- A message opens its Room, searches the existing history and highlights the
  target. A thread reply opens the exact thread, including its composer.
- Encrypted thread relationships must come from the decrypted message. Missing
  keys, inaccessible events and removed messages produce an error rather than
  silently choosing a different conversation.
- User links clicked inside a conversation continue to open the existing profile
  pane. Outside conversations, a user link explains where to open a profile;
  it does not create a direct chat.
- Unsupported actions and malformed Matrix links report an error inside Hagency.
  Ordinary web links retain their existing browser behavior.

Escape or Back cancels resolution. Dismissing the confirmation, opening another
link, navigating elsewhere, switching accounts or signing out retires the old
intent, including queued UI delivery. A join already submitted to the homeserver
may still complete after cancellation; cancelling navigation cannot undo it.

Resolution has a 30-second deadline. Old messages use the existing room/thread
history pagination and depend on server retention and this device's keys. This
change does not introduce Client's separate event-focused timeline architecture.

## Implementation and checks

The resolver and selected SDK/HTTP/Megolm tests are adapted from Hagency Client's
Matrix navigation implementation (`969b4e04a` and its root-delivery corrections).
Desktop retains its current dock, resource pool, daemon and approval architecture.
Its pinned SDK has no `WeakClient`; tickets retain the original SDK client and
also check Desktop's monotonically increasing client-slot revision, account epoch
and session identity. No dependency or lockfile changes are required.

`cargo test --profile fast --locked --lib matrix_links` exercises actual SDK
alias/event requests, join consent with original routing servers, original-session
replacement, delayed HTTP cancellation, actual Megolm inner/outer relations, and
native root/dock/composer delivery in wide and compact layouts. These tests also
run through `tools/check-hagency-regressions.py`.
