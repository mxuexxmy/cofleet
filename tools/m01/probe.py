#!/usr/bin/env python3
"""Local-only M0.1 authentication experiment. Not a production gateway."""

import argparse
import hashlib
import json
import secrets
import ssl
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs

parser = argparse.ArgumentParser()
parser.add_argument("--cert", required=True)
parser.add_argument("--key", required=True)
args = parser.parse_args()
lock = threading.RLock()
management, bootstraps, grants, sessions = {}, {}, {}, {}
revisions = {"one.cofleet.test": 1, "two.cofleet.test": 1}


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def token(store, value):
    raw = secrets.token_urlsafe(32)
    store[digest(raw)] = value
    return raw


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, value, cookies=()):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Referrer-Policy", "strict-origin")
        self.send_header("X-Content-Type-Options", "nosniff")
        for cookie in cookies:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()
        self.wfile.write(body)

    def cookie(self, name):
        parts = [
            part.strip().split("=", 1)
            for part in self.headers.get("Cookie", "").split(";")
        ]
        found = [part[1] for part in parts if len(part) == 2 and part[0] == name]
        return found[0] if len(found) == 1 else ""

    def set_cookie(self, name, value):
        return f"{name}={value}; Secure; HttpOnly; Path=/; SameSite=Lax"

    def do_GET(self):
        self.handle_request(False)

    def do_POST(self):
        self.handle_request(True)

    def handle_request(self, post):
        with lock:
            self.route(post)

    def route(self, post):
        now = time.monotonic()
        for store in (management, bootstraps, grants, sessions):
            for key in list(store):
                if store[key]["expires"] <= now:
                    del store[key]
        authority = self.headers.get("Host", "")
        port = self.server.server_port
        host = authority.split(":")[0]
        if authority != f"{host}:{port}" or host not in (
            "manager.cofleet.test",
            *revisions,
        ):
            return self.reply(404, {})
        mgr = f"https://manager.cofleet.test:{port}"
        body = {}
        if post:
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 < size <= 4096:
                return self.reply(400, {})
            try:
                raw = self.rfile.read(size).decode()
                body = (
                    json.loads(raw)
                    if self.headers.get("Content-Type", "").startswith(
                        "application/json"
                    )
                    else {k: v[0] for k, v in parse_qs(raw).items()}
                )
            except (ValueError, UnicodeError):
                return self.reply(400, {})
        if host == "manager.cofleet.test":
            if post and self.headers.get("Origin") != mgr:
                return self.reply(403, {})
            if self.path == "/login" and post:
                # Fixture identities only. No platform identity or production login is simulated.
                user = body.get("user")
                if user not in ("alice", "bob"):
                    return self.reply(400, {})
                raw = token(management, {"user": user, "expires": now + 300})
                return self.reply(
                    200, {}, [self.set_cookie("__Host-cofleet_manager", raw)]
                )
            sid = digest(self.cookie("__Host-cofleet_manager"))
            identity = management.get(sid)
            if not identity:
                return self.reply(401, {})
            if self.path == "/api/private" and not post:
                return self.reply(200, {"user": identity["user"]})
            if self.path == "/grant" and post:
                challenge = body.get("challenge", "")
                pending = bootstraps.get(challenge)
                if not pending or identity["user"] != "alice":
                    return self.reply(403, {})
                if pending.get("issued"):
                    return self.reply(409, {})
                pending["issued"] = True
                grant = token(
                    grants,
                    {
                        "host": pending["host"],
                        "bootstrap": challenge,
                        "manager": sid,
                        "revision": revisions[pending["host"]],
                        "expires": now + 30,
                    },
                )
                return self.reply(200, {"ticket": grant})
            if self.path == "/logout" and post:
                del management[sid]
                return self.reply(200, {})
            if self.path == "/disable" and post:
                if identity["user"] != "alice":
                    return self.reply(403, {})
                revisions["one.cofleet.test"] += 1
                return self.reply(200, {})
            return self.reply(404, {})
        if self.path.startswith("/api/") or self.path in (
            "/login",
            "/grant",
            "/logout",
            "/disable",
        ):
            return self.reply(404, {})
        if self.path == "/.cofleet/begin" and not post:
            raw = token(bootstraps, {"host": host, "expires": now + 60})
            return self.reply(
                200,
                {"challenge": digest(raw)},
                [self.set_cookie("__Host-cofleet_bootstrap", raw)],
            )
        if self.path == "/.cofleet/accept" and post:
            if self.headers.get("Origin") != mgr:
                return self.reply(403, {"reason": "origin"})
            grant = grants.pop(digest(body.get("ticket", "")), None)
            bootstrap = digest(self.cookie("__Host-cofleet_bootstrap"))
            if (
                not grant
                or grant["host"] != host
                or grant["bootstrap"] != bootstrap
                or grant["manager"] not in management
                or grant["revision"] != revisions[host]
            ):
                return self.reply(403, {"reason": "grant_binding"})
            bootstraps.pop(bootstrap, None)
            raw = token(sessions, {**grant, "expires": now + 120})
            return self.reply(
                200,
                {},
                [
                    self.set_cookie("__Host-cofleet_access", raw),
                    "__Host-cofleet_bootstrap=; Secure; HttpOnly; Path=/; Max-Age=0",
                ],
            )
        session = sessions.get(digest(self.cookie("__Host-cofleet_access")))
        if (
            not session
            or session["host"] != host
            or session["manager"] not in management
            or session["revision"] != revisions[host]
        ):
            return self.reply(401, {})
        return self.reply(200, {"application": host})


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(args.cert, args.key)
server.socket = context.wrap_socket(server.socket, server_side=True)
print(json.dumps({"port": server.server_port}), flush=True)
server.serve_forever()
