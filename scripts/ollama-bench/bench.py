"""CPU/GPU benchmark for Hagency's Ollama runtime. Python 3 stdlib only.

Usage: python3 bench.py [--url http://127.0.0.1:11434] [--runs 3] MODEL...
Writes bench-<timestamp>.json and prints a summary. Empty or timed-out output
is a failure, never a pass.
"""
import argparse, json, os, statistics, subprocess, sys, tempfile, time, urllib.request

TIMEOUT_S = 900
HISTORY = "\n".join(f"@user{i % 5}:lab: message {i} about the release plan, risks and owners." for i in range(220))
BUGGY = "def add(a, b):\n    return a - b\n"
TEST = "from calc import add\nassert add(2, 3) == 5\nassert add(-1, 1) == 0\nprint('ok')\n"
WRITE_FILE = {"type": "function", "function": {"name": "write_file", "description": "Create a file",
    "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
    "required": ["path", "content"]}}}
READ_ROOM = {"type": "function", "function": {"name": "read_room_history", "description": "Read recent room messages",
    "parameters": {"type": "object", "properties": {"limit": {"type": "integer"}}, "required": []}}}
POST = {"type": "function", "function": {"name": "post_message", "description": "Post a reply in the room",
    "parameters": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}}

def chat(url, model, messages, tools=None):
    body = {"model": model, "messages": messages, "stream": True, "think": False, "options": {"temperature": 0}}
    if tools: body["tools"] = tools
    req = urllib.request.Request(url + "/api/chat", json.dumps(body).encode(), {"Content-Type": "application/json"})
    start, first, text, calls, final = time.monotonic(), None, "", [], {}
    with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
        for line in resp:
            chunk = json.loads(line)
            msg = chunk.get("message", {})
            if first is None and (msg.get("content") or msg.get("tool_calls")): first = time.monotonic()
            text += msg.get("content", ""); calls += msg.get("tool_calls", []) or []
            if chunk.get("done"): final = chunk
    wall = time.monotonic() - start
    ev, ed = final.get("eval_count", 0), final.get("eval_duration", 0) or 1
    pc, pd = final.get("prompt_eval_count", 0), final.get("prompt_eval_duration", 0) or 1
    return {"ttft_s": (first - start) if first else None, "wall_s": wall, "out_tps": ev / (ed / 1e9),
            "prompt_tps": pc / (pd / 1e9), "out_tokens": ev, "prompt_tokens": pc}, text, calls

def task_chat(url, m):
    s, text, _ = chat(url, m, [{"role": "user", "content": "In about 120 words, explain what a code review is for."}])
    return s, len(text.split()) >= 40

def task_summary(url, m):
    s, text, _ = chat(url, m, [{"role": "user", "content": "Summarize this room in 5 bullets:\n" + HISTORY}])
    return s, text.count("\n") >= 2

def task_tool(url, m):
    s, _, calls = chat(url, m, [{"role": "user", "content": "Create hello.txt containing hi. Use the tool."}], [WRITE_FILE])
    ok = any(c["function"]["name"] == "write_file" and c["function"]["arguments"].get("path", "").endswith("hello.txt") for c in calls)
    return s, ok

def task_code_edit(url, m):
    prompt = f"calc.py:\n```python\n{BUGGY}```\nThis test fails:\n```python\n{TEST}```\nReply with only the full fixed calc.py in one python code block."
    s, text, _ = chat(url, m, [{"role": "user", "content": prompt}])
    text = text.split("</think>")[-1]
    code = text.split("```python")[-1].split("```")[0] if "```" in text else text
    with tempfile.TemporaryDirectory() as d:
        open(os.path.join(d, "calc.py"), "w").write(code)
        open(os.path.join(d, "test_calc.py"), "w").write(TEST)
        r = subprocess.run([sys.executable, "-S", "test_calc.py"], cwd=d, capture_output=True, timeout=30)
    return s, r.returncode == 0 and b"ok" in r.stdout

def task_room_tools(url, m):
    msgs = [{"role": "user", "content": "Read the room history, then post a one-line reply to the room."}]
    s1, _, calls = chat(url, m, msgs, [READ_ROOM, POST])
    if not any(c["function"]["name"] == "read_room_history" for c in calls): return s1, False
    msgs += [{"role": "assistant", "content": "", "tool_calls": calls},
             {"role": "tool", "content": HISTORY[:1500], "tool_name": "read_room_history"}]
    s2, _, calls = chat(url, m, msgs, [READ_ROOM, POST])
    s1["wall_s"] += s2["wall_s"]
    return s1, any(c["function"]["name"] == "post_message" and c["function"]["arguments"].get("text") for c in calls)

TASKS = [("chat", task_chat), ("summary_2k", task_summary), ("tool_call", task_tool),
         ("code_edit", task_code_edit), ("room_tools", task_room_tools)]

def pct(values, p):
    values = sorted(v for v in values if v is not None)
    return values[min(len(values) - 1, int(round(p * (len(values) - 1))))] if values else None

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--url", default="http://127.0.0.1:11434")
    ap.add_argument("--runs", type=int, default=3); ap.add_argument("models", nargs="+")
    a = ap.parse_args(); results = []
    for model in a.models:
        for name, fn in TASKS:
            for run in range(a.runs):
                try: stats, ok = fn(a.url, model)
                except Exception as e: stats, ok = {"error": repr(e)[:200]}, False
                results.append({"model": model, "task": name, "run": run, "ok": ok, **stats})
                print(json.dumps(results[-1]), flush=True)
    out = f"bench-{time.strftime('%Y%m%d-%H%M%S')}.json"
    json.dump({"host": os.uname().nodename, "cpus": os.cpu_count(), "results": results}, open(out, "w"), indent=1)
    print(f"\n{'model':22} {'task':11} ok  ttft_p50 ttft_p95 wall_p50 out_tps")
    for model in a.models:
        for name, _ in TASKS:
            rs = [r for r in results if r["model"] == model and r["task"] == name]
            f = lambda k, p=0.5: pct([r.get(k) for r in rs], p)
            fmt = lambda v: f"{v:8.1f}" if v is not None else "       -"
            print(f"{model:22} {name:11} {sum(r['ok'] for r in rs)}/{len(rs)} {fmt(f('ttft_s'))} {fmt(f('ttft_s', .95))} {fmt(f('wall_s'))} {fmt(f('out_tps'))}")
    print(f"\nwrote {out}")
    return 0 if all(r["ok"] for r in results) else 1

if __name__ == "__main__":
    sys.exit(main())
