# 0024. Agent replies sit in the room, not in a thread

Status: accepted (Chris, 2026-11-17); server half is hagency-server ADR 0004

## Context
When someone @mentioned an Agent in a group room, the Agent's run card, its
answer and its progress went into a Matrix thread under the question. The
desktop showed "dev room · Thread" and "Replying in a thread. Only people who
open this thread see these messages." Other members (Betty, in her own window
on another server) saw only the question in the main timeline; follow-ups
typed in the thread stayed hidden. Chris: "in WeChat, as long as you are in
the room, all conversations can be seen."

## Decision
1. **Room setting.** One state event, `im.hagency.agent_replies`
   (`state_key` ""), content `{"mode":"room"}` or `{"mode":"thread"}`. A room
   without it, or with a value this version does not know, is `room`. The
   room information pane's Overview shows "Agent replies: In the room / In
   threads"; writing it needs the room's state power level (a refusal is
   shown and the previous choice returns). The contract lives in
   `hagency_core::agent_replies` (desktop) and `agent_replies.rs`
   (hagency-server); both pin the event type in a test.
2. **Placement rule** (`agent_replies::placement`), the same on both sides:
   * top-level owner DM (thread root = room id): flat, no relation (as before);
   * a request a person wrote inside a thread (root != request): that thread,
     in both modes, so threads remain available when someone starts one and
     existing threaded history keeps working;
   * a top-level group request (root == request): `room` gives a
     main-timeline reply, `m.relates_to: {"m.in_reply_to": {"event_id": <request>}}`,
     never `m.thread`; `thread` gives the earlier thread under the request.
3. **What follows the rule.** The Agent device's encrypted answer
   (`placed_reply_content`), its unreadable-request notice, and the run card
   (`Card::content_in` and its edits). The server's plaintext answer and its
   request notices. The server's progress status was already a top-level
   notice replying to the request.
4. **Fallback for other clients.** A flat answer's plain `body` starts with
   `> <@requester> first line of the request` (80 characters at most) and a
   blank line; `formatted_body` carries no quote, so Element renders the quote
   once from the relation and plain-text clients still see it. Run-card edits
   carry no fallback; the edit's `m.new_content` repeats the card's relation
   as before, which clients ignore for a replacement. matrix-sdk-ui
   strips the fallback for the desktop, and the server's room context already
   strips reply fallbacks.
5. **Progress stays collapsed.** The run's progress messages stay in a thread
   under the request (they are working notes, not the conversation). The
   answer card's "N updates" now opens them **in place** inside the card in
   the main timeline ("Hide updates" closes them); it no longer navigates to
   the thread. `ShowRunUpdates` and the room screen's thread jump are removed.
   The list shows only the run Agent's own text messages in the thread, is
   keyed by account, room, root and Agent, and drops superseded or closed loads.
6. **Continuing the conversation.** An Agent answer has a Reply action (run
   cards included). A reply to an Agent's message in a group room no longer
   auto-mentions the Agent (`AddMentions::No`): quoting an answer must not
   wake the Agent for anyone. When the replier is the requester of the
   answered request, and that request mentioned exactly this one Agent, the
   composer shows "Replying to <agent>" (removable) and mentions the Agent:
   the same rule and widget as the thread follow-up
   (`thread_followup::followup_agent`, branch feat/thread-followup-mention).
   Quoting an Agent also routes to it (Octos `target_user_id`) only while
   that hint addresses it; a room timeline refresh keeps the reply's hint.
   The follow-up is an ordinary top-level mention, so the server's
   who-can-ask, consent and private-Agent rules apply unchanged, and its
   answer is again a flat reply to it. Its context is the room's recent
   history, which in this mode holds the earlier question and answer.

## Consequences
* Betty sees Anna's question, the Agent's answer and Anna's follow-up in the
  main timeline, in Hagency and in Element.
* A follow-up is a new request, not a turn in the earlier thread session:
  the Agent sees the conversation through room history (50 messages, 24 KiB
  in plaintext rooms; the device's own decrypted history in encrypted rooms).
* Not yet placed by the setting: question cards (still in the run's thread;
  the desktop draws them on the request row either way), pause notices, and
  the managed shared-execution path (`console/native/shared_execution`,
  `hagency-matrix` outgoing journal), whose receipts validate the thread
  relation. These keep the thread under the request until they move to the
  same rule.
* Changing the setting affects new answers only; earlier answers stay where
  they were posted.
