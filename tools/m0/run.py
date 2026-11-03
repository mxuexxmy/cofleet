#!/usr/bin/env python3
"""Opt-in real LazyCat M0 probe. Private state stays under .local/m0; no production upgrade.
Requires aiohttp for verify. Uses the existing logged-in desktop client, never saves its cookie.
"""

import argparse
import asyncio
import json
import os
import pathlib
import secrets
import shutil
import subprocess
import urllib.parse
import urllib.request
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[2]
BASE = ROOT / ".local/m0"
CLI = ["mise", "exec", "node@26.8.2", "--", "lzc-cli"]
BIN = ROOT / "target/x86_64-unknown-linux-musl/release/cofleet-access-probe"


def command(args, **kw):
    return subprocess.run(
        args, check=True, text=True, capture_output=True, **kw
    ).stdout.strip()


def cli(*args):
    return command(CLI + list(args))


def local_context():
    core = json.loads(
        (pathlib.Path.home() / ".config/lzc-client-desktop/core.cfg").read_text()
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def local(path):
        return json.load(
            opener.open(
                urllib.request.Request(
                    "http://127.0.0.1:" + str(core["http_port"]) + path,
                    headers=core["extra_headers"],
                ),
                timeout=10,
            )
        )

    box = local("/default-box")
    ext = local(
        "/box-ext-info?"
        + urllib.parse.urlencode({"url": "https://" + box["boxDomain"] + "/"})
    )
    cookie = next(c["value"] for c in ext["cookies"] if c["name"] == "Lzc-Auth-Token")
    return box, "Lzc-Auth-Token=" + cookie


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


def snapshot(cookie):
    manager = (ROOT / ".local/current-manager-url").read_text().strip()
    box, _ = local_context()
    assert urllib.parse.urlparse(manager).hostname.endswith("." + box["boxDomain"]), (
        "production manager is outside the default microserver"
    )
    opener = urllib.request.build_opener(NoRedirect())
    return {
        path: json.load(
            opener.open(
                urllib.request.Request(manager + path, headers={"Cookie": cookie}),
                timeout=15,
            )
        )
        for path in ["/api/v1/devices", "/api/v1/workloads"]
    }


def state():
    return json.loads((BASE / "current.json").read_text())


def target_cookie(saved):
    box, cookie = local_context()
    if box["boxDomain"] != saved["platform_domain"]:
        raise RuntimeError(
            "Default microserver changed; restore the original target before continuing M0"
        )
    return cookie


def prepare():
    if (BASE / "current.json").exists() and not state().get("cleaned"):
        raise RuntimeError(
            "An M0 run is already active; clean it up before preparing another"
        )
    box, cookie = local_context()
    run = uuid.uuid4().hex[:12]
    directory = BASE / run
    directory.mkdir(parents=True, mode=0o700)
    os.chmod(directory, 0o700)
    fixture = directory / "fixture"
    fixture.mkdir(mode=0o700)
    shutil.copyfile(ROOT / "tools/m0/fixture.py", fixture / "fixture.py")
    command(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            str(fixture / "key.pem"),
            "-out",
            str(fixture / "cert.pem"),
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
        ]
    )
    os.chmod(fixture / "key.pem", 0o600)
    password, session = secrets.token_urlsafe(24), secrets.token_urlsafe(24)
    envfile = directory / "fixture.env"
    envfile.write_text(
        "M0_PASSWORD="
        + password
        + "\nM0_SESSION="
        + session
        + "\nM0_COOKIE_DOMAIN=."
        + box["boxDomain"]
        + "\n"
    )
    os.chmod(envfile, 0o600)
    image = command(
        [
            "docker",
            "image",
            "inspect",
            os.environ.get("M0_FIXTURE_IMAGE", "cofleet-workspace:0.4.0"),
            "--format",
            "{{.Id}}",
        ]
    )
    name = "cofleet-m0-" + run
    command(
        [
            "docker",
            "run",
            "--detach",
            "--name",
            name,
            "--label",
            "xyz.mxue.cofleet.m0=" + run,
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--user",
            str(os.getuid()) + ":" + str(os.getgid()),
            "--memory=128m",
            "--cpus=0.5",
            "--pids-limit=64",
            "--env-file",
            str(envfile),
            "-v",
            str(fixture) + ":/fixture:ro",
            "-p",
            "127.0.0.1::8800",
            "-p",
            "127.0.0.1::8801",
            "-p",
            "127.0.0.1::8802",
            "--entrypoint",
            "python3",
            image,
            "/fixture/fixture.py",
        ]
    )
    info = json.loads(command(["docker", "inspect", name]))[0]
    ports = info["NetworkSettings"]["Ports"]
    port = lambda n: int(ports[str(n) + "/tcp"][0]["HostPort"])
    before = snapshot(cookie)
    owner = (
        before["/api/v1/devices"][0]["user"]
        if before["/api/v1/devices"] and "user" in before["/api/v1/devices"][0]
        else None
    )
    if owner is None:
        manager = (ROOT / ".local/current-manager-url").read_text().strip()
        owner = json.load(
            urllib.request.urlopen(
                urllib.request.Request(
                    manager + "/api/whoami", headers={"Cookie": cookie}
                ),
                timeout=10,
            )
        )["user"]
    package = "xyz.mxue.lzc.app.cofleet.m0." + run
    domain = "cofleetm0" + run + "." + box["boxDomain"]
    settings = {
        "domain": domain,
        "owner": owner,
        "key": secrets.token_urlsafe(32),
        "device": str(uuid.uuid4()),
        "login_port": port(8800),
        "static_port": port(8801),
        "tls_port": port(8802),
    }
    content = directory / "content"
    content.mkdir(mode=0o755)
    shutil.copyfile(BIN, content / "probe")
    os.chmod(content / "probe", 0o755)
    (content / "config.json").write_text(json.dumps(settings))
    os.chmod(content / "config.json", 0o644)
    (content / "start.sh").write_text(
        "#!/bin/sh\nset -eu\nexec /lzcapp/pkg/content/probe gateway --config /lzcapp/pkg/content/config.json\n"
    )
    os.chmod(content / "start.sh", 0o755)
    (directory / "package.yml").write_text(
        "package: "
        + package
        + '\nversion: "0.0.1"\nname: CoFleet M0 isolated probe\nmin_os_version: "1.6.0"\npermissions:\n  required: []\n  optional: []\n'
    )
    (directory / "lzc-manifest.yml").write_text(
        "application:\n  subdomain: cofleetm0"
        + run
        + '\n  run_as: "1000:1000"\n  routes:\n    - /=exec://9768,/lzcapp/pkg/content/start.sh\n'
    )
    shutil.copyfile(ROOT / "assets/cofleet-icon-256.png", directory / "icon.png")
    (directory / "lzc-build.yml").write_text(
        "manifest: ./lzc-manifest.yml\ncontentdir: ./content\nicon: ./icon.png\npkgout: ./\n"
    )
    (directory / "before.json").write_text(json.dumps(before))
    cfg = {
        "directory": str(directory),
        "package": package,
        "fixture": name,
        "settings": settings,
        "password": password,
        "session": session,
        "platform_domain": box["boxDomain"],
    }
    (BASE / "current.json").write_text(json.dumps(cfg))
    os.chmod(BASE / "current.json", 0o600)
    print("Prepared isolated M0 fixture and package; private state under .local/m0.")


def deploy():
    s = state()
    target_cookie(s)
    directory = pathlib.Path(s["directory"])
    out = directory / "probe.lpk"
    os.chmod(directory / "content", 0o755)
    os.chmod(directory / "content/config.json", 0o644)
    shutil.copyfile(BIN, directory / "content/probe")
    os.chmod(directory / "content/probe", 0o755)
    command(CLI + ["project", "build", "-o", str(out)], cwd=directory)
    # Never install the production package from this probe helper.
    assert s["package"].startswith("xyz.mxue.lzc.app.cofleet.m0.")
    print(cli("app", "install", str(out)).splitlines()[0])
    print("Isolated M0 package installed.")


async def verify():
    import aiohttp

    s = state()
    cfg = s["settings"]
    directory = pathlib.Path(s["directory"])
    base = "https://" + cfg["domain"]
    login = "https://login-" + cfg["domain"]
    static = "https://static-" + cfg["domain"]
    badtls = "https://badtls-" + cfg["domain"]
    cookie = target_cookie(s)
    node = subprocess.Popen(
        [
            str(BIN),
            "node",
            "--config",
            str(directory / "content/config.json"),
            "--node-config",
            str(ROOT / ".local/runtime/node"),
        ],
        stdout=(directory / "node.log").open("w"),
        stderr=subprocess.STDOUT,
    )
    checks = {}
    async with aiohttp.ClientSession(
        timeout=aiohttp.ClientTimeout(total=20), cookie_jar=aiohttp.DummyCookieJar()
    ) as client:

        async def req(url, auth=True, headers=None, method="GET", data=None):
            h = {"Cookie": cookie} if auth else {}
            h.update(headers or {})
            async with client.request(
                method, url, headers=h, data=data, allow_redirects=False
            ) as r:
                return (
                    r.status,
                    dict(r.headers),
                    await r.text(),
                    list(r.headers.getall("Set-Cookie", [])),
                )

        try:
            for _ in range(80):
                status, headers, body, _ = await req(base + "/api/m0/status")
                if (
                    status == 200
                    and headers.get("Content-Type", "").startswith("application/json")
                    and json.loads(body)["node_connected"]
                ):
                    break
                if node.poll() is not None:
                    raise AssertionError("node exited; inspect private node.log")
                await asyncio.sleep(0.5)
            assert status == 200, (status, body[:100])
            assert json.loads(body)["node_connected"]
            assert json.loads(body)["host"] == cfg["domain"]
            checks["exec_route_preserves_host"] = True
            assert (await req(static + "/"))[0] == 200
            checks["static_independent_origin"] = True
            status, _, body, _ = await req(
                login + "/headers",
                headers={
                    "Cookie": cookie
                    + "; cofleet_workspace=hub-sentinel; m0_marker=app",
                    "Authorization": "Bearer m0-application-token",
                    "X-HC-User-ID": "spoofed-user",
                    "X-HC-User-Ticket": "spoofed-ticket",
                    "X-Forwarded-For": "203.0.113.1",
                },
            )
            assert status == 200, status
            echo = json.loads(body)
            assert not echo["identity_leaked"]
            assert echo["cookie_names"] == ["m0_marker"], echo
            assert echo["authorization"] == "Bearer m0-application-token"
            assert echo["forwarded_for"] is None
            assert echo["host"] == "login-" + cfg["domain"]
            assert echo["forwarded_host"] == echo["host"]
            assert echo["forwarded_proto"] == "https"
            checks["application_headers_and_secret_filter"] = True
            for url in [base + "/api/m0/status", login + "/", static + "/"]:
                assert (
                    await req(url, auth=False, headers={"X-HC-User-ID": cfg["owner"]})
                )[0] in [302, 303, 307, 401, 403]
                assert (
                    await req(
                        url,
                        auth=False,
                        headers={"Cookie": "Lzc-Auth-Token=invalid-session"},
                    )
                )[0] in [302, 303, 307, 401, 403]
            checks["anonymous_spoofed_identity_and_invalid_login_rejected"] = True
            assert (await req("https://unknown-" + cfg["domain"] + "/"))[0] == 404
            checks["unknown_prefix_rejected"] = True
            for path in ["/api/m0/status", "/api/v1/devices", "/api/m0/node"]:
                assert (await req(login + path))[0] == 404
            assert (await req(base + "/api/m0/status", headers={"Origin": login}))[
                0
            ] == 403
            assert (await req(login + "/", headers={"Origin": static}))[0] == 403
            checks["management_host_and_origin_isolation"] = True
            status, _, _, cookies = await req(login + "/cookies")
            assert status == 200
            assert not any("forbidden" in value for value in cookies)
            appcookies = [
                v for v in cookies if v.startswith(("m0_domain=", "m0_second="))
            ]
            assert len(appcookies) == 2
            assert all(
                not any(
                    part.strip().lower().startswith("domain=")
                    for part in v.split(";")[1:]
                )
                for v in appcookies
            )
            checks["multiple_host_only_cookies_and_reserved_names"] = True
            status, headers, _, _ = await req(login + "/redirect/local")
            assert status == 302 and headers["Location"] == login + "/session"
            assert (await req(login + "/redirect/relative"))[1][
                "Location"
            ] == "/session"
            assert (await req(login + "/redirect/external"))[1][
                "Location"
            ] == "https://example.invalid/login"
            assert (await req(login + "/redirect/mismatch"))[0] == 502
            checks["redirect_mapping_and_invalid_private_redirect"] = True
            assert (await req(login + "/not-supported-base"))[0] == 404
            checks["unsupported_application_path_reported"] = True
            status, _, _, cookies = await req(
                login + "/login", method="POST", data={"password": s["password"]}
            )
            assert status == 303
            assert any(v.startswith("m0_session=") for v in cookies)
            app_cookie = cookie + "; m0_session=" + s["session"]
            assert (await req(login + "/session", headers={"Cookie": app_cookie}))[
                0
            ] == 200
            checks["application_login_session"] = True
            assert (await req(login + "/api/session", headers={"Cookie": app_cookie}))[
                0
            ] == 200
            checks["application_api_namespace_preserved"] = True
            ws = await client.ws_connect(
                login + "/ws", headers={"Cookie": app_cookie}, origin=login
            )
            await ws.send_str("m0-echo")
            message = await ws.receive(timeout=10)
            assert message.data == "m0-echo"
            checks["authenticated_websocket_echo"] = True
            for h, origin in [
                ({"Cookie": "m0_session=" + s["session"]}, login),
                ({"Cookie": app_cookie}, static),
            ]:
                try:
                    await client.ws_connect(login + "/ws", headers=h, origin=origin)
                    raise AssertionError("unauthorized websocket accepted")
                except aiohttp.WSServerHandshakeError as e:
                    assert e.status in [302, 303, 307, 401, 403] or (
                        e.status == 200
                        and e.request_info.real_url.host == s["platform_domain"]
                        and e.request_info.real_url.path == "/sys/login"
                    )
            checks["websocket_login_and_origin_rejection"] = True
            assert (await req(badtls + "/"))[0] == 502
            checks["self_signed_upstream_certificate_rejected"] = True
            node.terminate()
            node.wait(timeout=10)
            closed = await ws.receive(timeout=12)
            assert closed.type in [
                aiohttp.WSMsgType.CLOSE,
                aiohttp.WSMsgType.CLOSED,
                aiohttp.WSMsgType.ERROR,
            ]
            await ws.close()
            checks["node_session_disconnect_closes_active_stream"] = True
            assert (await req(login + "/"))[0] == 503
            checks["disconnected_node_rejected"] = True
            report = {
                "stage": "M0",
                "checks": checks,
                "real_platform_accounts_tested": 1,
                "limitations": [
                    "No second real platform account was available; owner rejection is separately unit-tested at the trusted boundary.",
                    "Removing or invalidating the client cookie was tested; no real platform logout/revocation was triggered for the existing user.",
                    "Services are disposable fixed-port fixtures; no generic container discovery or persisted endpoints are implemented.",
                ],
            }
            (directory / "verification.json").write_text(json.dumps(report, indent=2))
            print(json.dumps(report, ensure_ascii=False))
        finally:
            if node.poll() is None:
                node.terminate()
                node.wait(timeout=10)


def cleanup():
    s = state()
    cookie = target_cookie(s)
    assert s["package"].startswith("xyz.mxue.lzc.app.cofleet.m0.") and s[
        "fixture"
    ].startswith("cofleet-m0-")
    label = command(
        [
            "docker",
            "inspect",
            s["fixture"],
            "--format",
            '{{index .Config.Labels "xyz.mxue.cofleet.m0"}}',
        ]
    )
    assert label == pathlib.Path(s["directory"]).name, "fixture ownership mismatch"
    try:
        cli("app", "uninstall", s["package"])
    finally:
        command(["docker", "rm", "-f", s["fixture"]])
    s["cleaned"] = True
    (BASE / "current.json").write_text(json.dumps(s))
    after = snapshot(cookie)
    before = json.loads((pathlib.Path(s["directory"]) / "before.json").read_text())
    old = {d["device_id"] for d in before["/api/v1/devices"] if d["online"]}
    new = {d["device_id"] for d in after["/api/v1/devices"] if d["online"]}
    assert old <= new
    assert before["/api/v1/workloads"] == after["/api/v1/workloads"]
    new_devices = {d["device_id"]: d for d in after["/api/v1/devices"]}
    for old_device in before["/api/v1/devices"]:
        if not old_device["online"]:
            continue
        new_device = new_devices[old_device["device_id"]]
        for field in ["resource_policy", "accepting_tasks", "capabilities"]:
            assert old_device.get(field) == new_device.get(field), field
        if old_device.get("workspace"):
            for field in ["volume", "project_path", "desired_running"]:
                assert old_device["workspace"].get(field) == new_device[
                    "workspace"
                ].get(field)
        if old_device.get("pipeline"):
            for field in ["current", "previous"]:
                old_release = old_device["pipeline"].get(field)
                if old_release:
                    new_release = new_device["pipeline"].get(field)
                    assert (
                        new_release
                        and old_release["id"] == new_release["id"]
                        and old_release["image_id"] == new_release["image_id"]
                    )
    report = {
        "temporary_package_removed": True,
        "temporary_container_removed": True,
        "production_online_devices_retained": len(old),
        "production_plans_retained": True,
        "production_configuration_and_release_ids_retained": True,
    }
    (pathlib.Path(s["directory"]) / "cleanup-verification.json").write_text(
        json.dumps(report, indent=2)
    )
    print(json.dumps(report))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["prepare", "deploy", "verify", "cleanup"])
    args = parser.parse_args()
    if args.action == "verify":
        asyncio.run(verify())
    else:
        globals()[args.action]()
