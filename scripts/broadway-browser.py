"""Headless stand-in for the browser that gtk4-broadwayd expects.

Broadway only drives a frame once its client acknowledges the previous one, so
with no browser attached the GDK frame clock stalls at about one frame a
second and a mapped window never takes a resize. This connects to the
daemon's HTTP port, reports a screen of WIDTH x HEIGHT, and answers the two
things a browser answers: surface moves/resizes (CONFIGURE_NOTIFY) and
roundtrips (ROUNDTRIP_NOTIFY). It draws nothing.

usage: broadway-browser.py PORT WIDTH HEIGHT
"""
import base64
import os
import socket
import struct
import sys
import time

# Server -> client ops (broadway-protocol.h) and client -> server events.
OP_GRAB, OP_UNGRAB, OP_NEW, OP_SHOW, OP_HIDE, OP_RAISE, OP_LOWER, OP_DESTROY = range(8)
OP_MOVE_RESIZE, OP_TRANSIENT, OP_DISCONNECTED, OP_UPDATE, OP_KEYBOARD = 8, 9, 10, 11, 12
OP_UPLOAD, OP_RELEASE, OP_NODES, OP_ROUNDTRIP = 13, 14, 15, 16
EV_CONFIGURE, EV_SCREEN, EV_ROUNDTRIP = 11, 12, 14


class Link:
    def __init__(self, port):
        for attempt in range(100):  # the daemon may not be listening yet
            try:
                self.sock = socket.create_connection(("127.0.0.1", port))
                break
            except ConnectionRefusedError:
                if attempt == 99:
                    raise
                time.sleep(0.05)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall(
            (
                "GET /socket HTTP/1.1\r\nHost: 127.0.0.1:%d\r\nUpgrade: websocket\r\n"
                "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
                "Sec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: broadway\r\n\r\n"
            % (port, key)
            ).encode()
        )
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise SystemExit("broadway-browser: handshake closed")
            head += chunk
        head, self.buf = head.split(b"\r\n\r\n", 1)
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise SystemExit("broadway-browser: no websocket upgrade")
        self.serial = 0

    def need(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(1 << 16)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def frame(self):
        """The next binary message, continuation frames joined."""
        data = b""
        while True:
            b0, b1 = self.need(2)
            size = b1 & 0x7F
            if size == 126:
                (size,) = struct.unpack(">H", self.need(2))
            elif size == 127:
                (size,) = struct.unpack(">Q", self.need(8))
            payload = self.need(size)
            opcode = b0 & 0x0F
            if opcode == 8:
                raise EOFError
            if opcode in (0, 1, 2):
                data += payload
                if b0 & 0x80:
                    return data

    def send(self, event, *args):
        msg = struct.pack(">%di" % (3 + len(args)), event, self.serial, int(time.time() * 1000) & 0x7FFFFFFF, *args)
        mask = os.urandom(4)
        head = bytes([0x82, 0x80 | len(msg)])
        self.sock.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(msg)))


def main(port, width, height):
    link = Link(port)
    surfaces = {}
    link.send(EV_SCREEN, width, height, 1)
    print("ready", flush=True)
    while True:
        msg = link.frame()
        pos = 0

        def take(fmt):
            nonlocal pos
            size = struct.calcsize(fmt)
            vals = struct.unpack_from(fmt, msg, pos)
            pos += size
            return vals if len(vals) > 1 else vals[0]

        while pos < len(msg):
            op = take("<B")
            link.serial = take("<I")
            if op == OP_NEW:
                sid, x, y, w, h = take("<HhhHH")
                surfaces[sid] = [x, y, w, h]
            elif op in (OP_SHOW, OP_HIDE, OP_RAISE, OP_LOWER, OP_DESTROY):
                sid = take("<H")
                if op == OP_DESTROY:
                    surfaces.pop(sid, None)
            elif op == OP_TRANSIENT:
                take("<HH")
            elif op == OP_ROUNDTRIP:
                sid, tag = take("<HI")
                link.send(EV_ROUNDTRIP, sid, tag)
            elif op == OP_MOVE_RESIZE:
                sid = take("<H")
                flags = take("<B")
                s = surfaces.setdefault(sid, [0, 0, 0, 0])
                if flags & 1:
                    s[0], s[1] = take("<hh")
                if flags & 2:
                    s[2], s[3] = take("<HH")
                link.send(EV_CONFIGURE, sid, *s)
            elif op == OP_UPLOAD:
                take("<I")
                n = take("<I")
                pos += n
            elif op == OP_RELEASE:
                take("<I")
            elif op == OP_NODES:
                take("<H")
                n = take("<I")
                pos += 4 * n
            elif op == OP_GRAB:
                take("<HB")
            elif op == OP_UNGRAB:
                pass
            elif op == OP_KEYBOARD:
                take("<H")
            elif op == OP_DISCONNECTED:
                return
            else:
                raise SystemExit("broadway-browser: unknown op %d" % op)


if __name__ == "__main__":
    try:
        main(int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]))
    except (EOFError, ConnectionError):
        pass
