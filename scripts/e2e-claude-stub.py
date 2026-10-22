#!/usr/bin/env python3
"""Stub of the Anthropic Messages API for the Claude Code E2E (CC6, CC4).

Standard library only, loopback only, no key checked, no network. Claude
Code points at it through ANTHROPIC_BASE_URL. A turn that offers tools gets,
by the text of the user's turn (every user message after the last assistant
one):

- `handoff agentcenter-op:<uuid>`: one `Write` of
  `<STUB_HANDOFF_DIR>/e2e-<uuid>.md` (synthetic text);
- `pickup <absolute path>`: one `Read` of that path;
- `e2e warm-up`: one `Write` of `<STUB_GRANT_PATH>`
  (the script grants the session the project's .claude/ folder on it);
- a `tool_result` in the turn: a short closing text;
- anything else (and every turn without tools): a short text.

Usage: e2e-claude-stub.py <port-file> <log-file>. The port it bound on
127.0.0.1 is written to <port-file>; the log has one line per request with
method, path, tools offered and the reply kind, never message bodies.
For debugging only, STUB_DUMP=<file> appends each request's tool names and
messages to <file> (opt-in; keep it inside the run's private root).
"""
import json
import os
import re
import sys
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HANDOFF_DIR = os.environ["STUB_HANDOFF_DIR"]
GRANT_PATH = os.environ["STUB_GRANT_PATH"]
OP = re.compile(r"handoff agentcenter-op:([0-9a-f-]{36})")
PICKUP = re.compile(r"pickup (/[^\s<>]+)")
LOG = None


def texts(message):
    content = message.get("content")
    if isinstance(content, str):
        return [content]
    out = []
    for b in content or []:
        if b.get("type") == "text":
            out.append(b.get("text", ""))
    return out


def decide(body):
    messages = body.get("messages") or []
    tools = {t.get("name") for t in body.get("tools") or []}
    if not messages or not ({"Write", "Read"} <= tools):
        return "text", None
    # The user's turn may span several user messages (prompt, then reminders
    # and attachments), and Claude Code 2.1 may end the list with a `system`
    # role message (skill listing, context): read the user messages back to
    # the last assistant one, ignoring system ones.
    turn = []
    for m in reversed(messages):
        if m.get("role") == "system":
            continue
        if m.get("role") != "user":
            break
        content = m.get("content")
        if isinstance(content, list) and any(b.get("type") == "tool_result" for b in content):
            return "text", None
        turn[:0] = texts(m)
    text = "\n".join(turn)
    m = OP.search(text)
    if m:
        path = os.path.join(HANDOFF_DIR, f"e2e-{m.group(1)}.md")
        body = f"# Handoff E2E\n\nsynthetic handoff for operation {m.group(1)}\n"
        return "Write", {"file_path": path, "content": body}
    if "e2e warm-up" in text:
        return "Write", {"file_path": GRANT_PATH, "content": "e2e grant\n"}
    m = PICKUP.search(text)
    if m:
        return "Read", {"file_path": m.group(1)}
    return "text", None


def events(model, kind, args):
    mid = "msg_" + uuid.uuid4().hex[:24]
    yield "message_start", {
        "type": "message_start",
        "message": {
            "id": mid, "type": "message", "role": "assistant", "model": model,
            "content": [], "stop_reason": None, "stop_sequence": None,
            "usage": {"input_tokens": 10, "output_tokens": 1,
                      "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
        },
    }
    if kind == "text":
        yield "content_block_start", {"type": "content_block_start", "index": 0,
                                      "content_block": {"type": "text", "text": ""}}
        yield "content_block_delta", {"type": "content_block_delta", "index": 0,
                                      "delta": {"type": "text_delta", "text": "ok."}}
        stop = "end_turn"
    else:
        tid = "toolu_" + uuid.uuid4().hex[:24]
        yield "content_block_start", {"type": "content_block_start", "index": 0,
                                      "content_block": {"type": "tool_use", "id": tid,
                                                        "name": kind, "input": {}}}
        yield "content_block_delta", {"type": "content_block_delta", "index": 0,
                                      "delta": {"type": "input_json_delta",
                                                "partial_json": json.dumps(args)}}
        stop = "tool_use"
    yield "content_block_stop", {"type": "content_block_stop", "index": 0}
    yield "message_delta", {"type": "message_delta",
                            "delta": {"stop_reason": stop, "stop_sequence": None},
                            "usage": {"output_tokens": 5}}
    yield "message_stop", {"type": "message_stop"}


def whole(model, kind, args):
    block = ({"type": "text", "text": "ok."} if kind == "text" else
             {"type": "tool_use", "id": "toolu_" + uuid.uuid4().hex[:24], "name": kind,
              "input": args})
    return {"id": "msg_" + uuid.uuid4().hex[:24], "type": "message", "role": "assistant",
            "model": model, "content": [block],
            "stop_reason": "end_turn" if kind == "text" else "tool_use",
            "stop_sequence": None, "usage": {"input_tokens": 10, "output_tokens": 5}}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def note(self, line):
        LOG.write(line + "\n")
        LOG.flush()

    def reply(self, code, obj):
        data = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.note(f"GET {self.path}")
        self.reply(200, {"data": [], "has_more": False})

    def do_HEAD(self):
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(n)
        try:
            body = json.loads(raw or b"{}")
        except ValueError:
            body = {}
        path = self.path.split("?")[0]
        if path.endswith("/count_tokens"):
            self.note(f"POST {path}")
            return self.reply(200, {"input_tokens": 10})
        if not path.endswith("/v1/messages"):
            self.note(f"POST {path} other")
            return self.reply(200, {})
        if os.environ.get("STUB_DUMP"):
            with open(os.environ["STUB_DUMP"], "a", encoding="utf-8") as f:
                f.write(json.dumps({"tools": [t.get("name") for t in body.get("tools") or []],
                                    "messages": body.get("messages")}) + "\n")
        model = body.get("model") or "claude-stub"
        kind, args = decide(body)
        tools = len(body.get("tools") or [])
        self.note(f"POST {path} model={model} tools={tools} stream={bool(body.get('stream'))} "
                  f"reply={kind}")
        if not body.get("stream"):
            return self.reply(200, whole(model, kind, args))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        for name, data in events(model, kind, args):
            self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode())
            self.wfile.flush()
        self.close_connection = True


def main():
    global LOG
    port_file, log_file = sys.argv[1], sys.argv[2]
    LOG = open(log_file, "a", encoding="utf-8")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(port_file + ".tmp", "w", encoding="utf-8") as f:
        f.write(str(server.server_address[1]))
    os.replace(port_file + ".tmp", port_file)
    server.serve_forever()


if __name__ == "__main__":
    main()
