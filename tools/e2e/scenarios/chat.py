"""Scenarios 1–4: answering, cross-server, thread follow-up, @room."""
import time

from hx import personas
from hx.scenario import (FAIL, PASS, Result, agent_messages, blocked, first_reply, mention_content,
                         now_ms, plain_content, seen_on, wait_for)
from hx import config

ANSWER_TIMEOUT_S = 240
SILENCE_WINDOW_S = 75  # how long "no agent run" is watched before it counts as silence


def _ask(ctx, asker: str, agent: str, label: str, thread_root: str = ""):
    token = f"E2E-{ctx.run_id}-{label}"
    text = f"{config.TEST_MARK} automated test: reply with exactly the word {token} and nothing else."
    since = now_ms()
    event = personas.persona(asker).send(ctx.room, mention_content(text, [agent], thread_root))
    return token, since, event


def _check_answer(result: Result, ctx, asker, agent, label, viewers, thread_root=""):
    """Ask, wait for the answer, check every viewer's server sees it."""
    token, since, event = _ask(ctx, asker, agent, label, thread_root)
    result.evidence.append(f"{label}: ask {event} by {asker}")
    reply = first_reply(asker, ctx.room, agent, since, ANSWER_TIMEOUT_S)
    if reply is None:
        result.note += f"{label}: no answer from {agent} in {ANSWER_TIMEOUT_S}s (asked {event}). "
        return False, event
    latency = (reply["origin_server_ts"] - since) / 1000
    result.evidence.append(f"{label}: reply {reply['event_id']} after {latency:.1f}s")
    ok = token in reply["content"].get("body", "")
    if not ok:
        result.note += f"{label}: reply lacks {token}: {reply['content'].get('body', '')[:120]!r}. "
    for viewer in viewers:
        if not wait_for(lambda v=viewer: seen_on(v, ctx.room, reply["event_id"]), 60):
            result.note += f"{label}: reply not visible from {viewer}'s server. "
            ok = False
    if thread_root and reply["content"].get("m.relates_to", {}).get("event_id") != thread_root:
        result.note += f"{label}: reply not in the thread {thread_root}. "
        ok = False
    return ok, event


def s1_mention_answer(ctx) -> Result:
    name = "1 @mention answer + latency"
    if (b := blocked(name, ctx, ["anna-claude"])):
        return b
    r, t0 = Result(name), time.time()
    ok, _ = _check_answer(r, ctx, "zz-anna", "anna-claude", "s1", ["zz-anna", "zz-betty"])
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def s2_cross_server(ctx) -> Result:
    name = "2 cross-server ask (beta→alpha agent, alpha→beta agent)"
    if (b := blocked(name, ctx, ["anna-claude", "bird-claude"])):
        return b
    r, t0 = Result(name), time.time()
    a, _ = _check_answer(r, ctx, "zz-betty", "anna-claude", "s2a", ["zz-anna", "zz-betty"])
    b2, _ = _check_answer(r, ctx, "zz-anna", "bird-claude", "s2b", ["zz-anna", "zz-betty"])
    ok = a and b2
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def _silent(ctx, agents, since, window_s) -> list:
    """Agent messages that appeared during the window (should be none)."""
    time.sleep(window_s)
    spoke = []
    for agent in agents:
        spoke += agent_messages("zz-anna", ctx.room, agent, since)
    return spoke


def s3_thread_followup(ctx) -> Result:
    name = "3 thread follow-up (mention auto-added) and no-mention ignored"
    if (b := blocked(name, ctx, ["bird-codex"])):
        return b
    r, t0 = Result(name), time.time()
    first, root = _check_answer(r, ctx, "zz-will", "bird-codex", "s3-root", ["zz-will"])
    followed = first and _check_answer(r, ctx, "zz-will", "bird-codex", "s3-thread", ["zz-anna"], root)[0]
    since = now_ms()
    ev = personas.persona("zz-will").send(ctx.room, plain_content(
        f"{config.TEST_MARK} thanks, no reply needed", thread_root=root))
    r.evidence.append(f"s3-nomention: {ev}")
    spoke = [e for e in _silent(ctx, ["bird-codex"], since, SILENCE_WINDOW_S)
             if e.get("content", {}).get("m.relates_to", {}).get("event_id") == root]
    if spoke:
        r.note += f"agent answered a no-mention follow-up: {spoke[0]['event_id']}. "
    ok = first and followed and not spoke
    r.status, r.seconds, r.kind = (PASS if ok else FAIL), time.time() - t0, "" if ok else "finding"
    return r


def s4_room_mention(ctx) -> Result:
    name = "4 @room starts no agent run"
    present = [a for a in config.AGENTS if ctx.agent_in_room(a)]
    if not present:
        return blocked(name, ctx, list(config.AGENTS))
    r, t0 = Result(name), time.time()
    since = now_ms()
    ev = personas.persona("zz-anna").send(ctx.room, plain_content(
        f"@room {config.TEST_MARK} announcement test, agents must stay quiet", room_mention=True))
    r.evidence.append(f"@room event {ev}; watched {SILENCE_WINDOW_S}s; agents present: {', '.join(present)}")
    spoke = _silent(ctx, present, since, SILENCE_WINDOW_S)
    if spoke:
        r.note = f"agents spoke after @room: {[e['sender'] + ' ' + e['event_id'] for e in spoke]}"
    r.status, r.seconds, r.kind = (FAIL, time.time() - t0, "finding") if spoke else (PASS, time.time() - t0, "")
    return r
