#!/usr/bin/env python3
"""Stub of an OpenAI-compatible chat completions API for the Pi E2E (CC5).

Standard library only, loopback only, no key checked, no network. The private
Pi points at it through the `stub` provider of HOME/.pi/agent/models.json.
Every request gets one short streamed text: a synthetic handoff when the last
user message asks for one (the extension's handoff prompt), otherwise a short
turn.

Usage: e2e-pi-stub.py <port-file> <log-file>. The port it bound on 127.0.0.1
is written to <port-file>; the log has one JSON line per request with the path,
the message count, the reply kind and two booleans over the last message of
a plain turn (never message bodies; a handoff prompt quotes the transcript, so
it is not flagged): `pickup_command` (the text `/agentcenter-pickup` was sent
as a prompt, i.e. the command was diverted) and `draft_marker` (the E2E draft
text was sent as a prompt).
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT_FILE, LOG_FILE = sys.argv[1:3]
DRAFT_MARKER = "e2e-draft-not-a-command"


def text_of(content):
    if isinstance(content, list):
        return "".join(p.get("text", "") for p in content if isinstance(p, dict))
    return content or ""


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        messages = body.get("messages", [])
        texts = [text_of(m.get("content")) for m in messages if isinstance(m, dict)]
        last = texts[-1] if texts else ""
        handoff = "Create a work handoff" in last
        reply = (
            "E2E-HANDOFF-cc5 Verified handoff. Next: continue."
            if handoff
            else "Synthetic turn."
        )
        prompt = "" if handoff else last
        with open(LOG_FILE, "a") as f:
            f.write(
                json.dumps(
                    {
                        "path": self.path,
                        "messages": len(messages),
                        "kind": "handoff" if handoff else "turn",
                        "pickup_command": "/agentcenter-pickup" in prompt,
                        "draft_marker": DRAFT_MARKER in prompt,
                    }
                )
                + "\n"
            )
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()

        def event(data):
            self.wfile.write(b"data: " + json.dumps(data).encode() + b"\n\n")
            self.wfile.flush()

        base = {
            "id": "e2e",
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": "fake",
        }
        delta = {"role": "assistant", "content": reply}
        event({**base, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]})
        usage = {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        event({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": usage})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(PORT_FILE, "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
