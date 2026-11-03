#!/usr/bin/env python3
"""Disposable, stdlib-only externally started Web app for M0. No Docker control."""

import base64
import hashlib
import http.cookies
import json
import os
import ssl
import struct
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SESSION = os.environ["M0_SESSION"]
PASSWORD = os.environ["M0_PASSWORD"]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, status, body=b"", headers=()):
        body = body.encode() if isinstance(body, str) else body
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def cookies(self):
        c = http.cookies.SimpleCookie()
        c.load(self.headers.get("Cookie", ""))
        return c

    def authenticated(self):
        return (
            self.cookies().get("m0_session") is not None
            and self.cookies()["m0_session"].value == SESSION
        )

    def do_POST(self):
        from urllib.parse import parse_qs

        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.path == "/login" and parse_qs(body.decode()).get("password") == [
            PASSWORD
        ]:
            self.reply(
                303,
                headers=[
                    ("Location", "/"),
                    (
                        "Set-Cookie",
                        "m0_session="
                        + SESSION
                        + "; Path=/; HttpOnly; Secure; SameSite=Lax",
                    ),
                ],
            )
            return
        self.reply(403, "Application login rejected")

    def do_GET(self):
        if self.server.mode == "static":
            self.reply(
                200,
                "<!doctype html><title>M0 static</title><h1>M0 static service</h1>",
                [("Content-Type", "text/html")],
            )
            return
        if self.path == "/headers":
            c = self.cookies()
            data = {
                "host": self.headers.get("Host"),
                "origin": self.headers.get("Origin"),
                "cookie_names": sorted(c.keys()),
                "authorization": self.headers.get("Authorization"),
                "identity_leaked": any(
                    k.lower().startswith(("x-hc-", "x-lzc", "x_lzc", "x-cofleet"))
                    for k in self.headers
                ),
                "forwarded_for": self.headers.get("X-Forwarded-For"),
                "forwarded_host": self.headers.get("X-Forwarded-Host"),
                "forwarded_proto": self.headers.get("X-Forwarded-Proto"),
            }
            self.reply(200, json.dumps(data), [("Content-Type", "application/json")])
            return
        if self.path == "/cookies":
            self.reply(
                200,
                "cookies",
                [
                    (
                        "Set-Cookie",
                        "m0_domain=kept; Domain="
                        + os.environ["M0_COOKIE_DOMAIN"]
                        + "; Path=/; HttpOnly; Secure; SameSite=Lax",
                    ),
                    ("Set-Cookie", "m0_second=kept; Path=/; Secure"),
                    (
                        "Set-Cookie",
                        "HC-Auth-Token=forbidden; Domain="
                        + os.environ["M0_COOKIE_DOMAIN"]
                        + "; Path=/",
                    ),
                    ("Set-Cookie", "Lzc-Auth-Token=forbidden; Path=/"),
                    ("Set-Cookie", "cofleet_workspace=forbidden; Path=/"),
                ],
            )
            return
        if self.path == "/redirect/local":
            self.reply(
                302,
                headers=[
                    (
                        "Location",
                        "http://127.0.0.1:" + str(self.server.server_port) + "/session",
                    )
                ],
            )
            return
        if self.path == "/redirect/external":
            self.reply(302, headers=[("Location", "https://example.invalid/login")])
            return
        if self.path == "/redirect/mismatch":
            self.reply(302, headers=[("Location", "http://127.0.0.1:1/not-configured")])
            return
        if self.path == "/redirect/relative":
            self.reply(302, headers=[("Location", "/session")])
            return
        if self.path in ("/session", "/api/session"):
            self.reply(
                200 if self.authenticated() else 401,
                json.dumps({"application_logged_in": self.authenticated()}),
                [("Content-Type", "application/json")],
            )
            return
        if self.path == "/ws":
            if not self.authenticated() or self.headers.get(
                "Origin"
            ) != "https://" + self.headers.get("Host", ""):
                self.reply(403, "WebSocket login or origin rejected")
                return
            key = self.headers.get("Sec-WebSocket-Key", "")
            accept = base64.b64encode(
                hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
                ).digest()
            ).decode()
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
            try:
                while True:
                    h = self.rfile.read(2)
                    if len(h) != 2:
                        break
                    opcode = h[0] & 15
                    length = h[1] & 127
                    if length == 126:
                        length = struct.unpack("!H", self.rfile.read(2))[0]
                    elif length == 127:
                        length = struct.unpack("!Q", self.rfile.read(8))[0]
                    if length > 4096:
                        break
                    mask = self.rfile.read(4) if h[1] & 128 else None
                    data = self.rfile.read(length)
                    if mask:
                        data = bytes(v ^ mask[i % 4] for i, v in enumerate(data))
                    if opcode == 8:
                        break
                    if opcode in (1, 9):
                        self.wfile.write(
                            bytes([0x80 | (10 if opcode == 9 else 1)])
                            + (
                                bytes([len(data)])
                                if len(data) < 126
                                else b"\x7e" + struct.pack("!H", len(data))
                            )
                            + data
                        )
                        self.wfile.flush()
            except (OSError, ValueError):
                pass
            self.close_connection = True
            return
        if self.path != "/":
            self.reply(
                404,
                "Application path not configured; set its public base URL in the original manager.",
            )
            return
        if not self.authenticated():
            page = '<!doctype html><title>M0 application login</title><h1>Application login</h1><form action="/login" method="post"><label>Password <input name="password" type="password"></label><button>Sign in</button></form>'
        else:
            page = """<!doctype html><title>M0 signed in</title><h1>Application signed in</h1><p id="ws">Connecting</p><script>const ws=new WebSocket('wss://'+location.host+'/ws');ws.onopen=()=>ws.send('m0-echo');ws.onmessage=e=>document.querySelector('#ws').textContent=e.data;ws.onerror=()=>document.querySelector('#ws').textContent='failed';</script>"""
        self.reply(200, page, [("Content-Type", "text/html; charset=utf-8")])


servers = []
for port, mode in [(8800, "login"), (8801, "static"), (8802, "tls")]:
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    server.mode = mode
    if mode == "tls":
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain("/fixture/cert.pem", "/fixture/key.pem")
        server.socket = context.wrap_socket(server.socket, server_side=True)
    servers.append(server)
    threading.Thread(target=server.serve_forever, daemon=True).start()
threading.Event().wait()
