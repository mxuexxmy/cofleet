#!/usr/bin/env python3
"""Linux desktop integration via the public StatusNotifier/DBusMenu APIs."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import json

BIN = Path(os.environ.get('COFLEET_BIN_DIR', 'target/debug')).resolve()

def call(service, path, method, *args):
    return subprocess.check_output(['gdbus', 'call', '--session', '--dest', service, '--object-path', path, '--method', method, '--', *map(str, args)], text=True, timeout=5)

def items():
    value = call('org.kde.StatusNotifierWatcher', '/StatusNotifierWatcher', 'org.freedesktop.DBus.Properties.Get', 'org.kde.StatusNotifierWatcher', 'RegisteredStatusNotifierItems')
    return set(re.findall(r"'([^']+)'", value))

def wait_for(fn):
    end = time.monotonic() + 15
    while time.monotonic() < end:
        result = fn()
        if result:
            return result
        time.sleep(.2)
    raise AssertionError('desktop condition timed out')

with tempfile.TemporaryDirectory(prefix='cf-ui-') as temp:
    data = Path(temp) / 'node'
    env = dict(os.environ, COFLEET_TOKEN='cofleet-tray-integration-token')
    def ctl(*args):
        return subprocess.check_output([BIN/'cofleetctl', '--data-dir', data, *args], env=env, text=True, timeout=5)
    ctl('init', '--name', 'tray-integration')
    before = items()
    agent = subprocess.Popen([BIN/'cofleet-agent', '--data-dir', data], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    tray = None
    try:
        wait_for(lambda: (data/'agent.sock').exists())
        tray = subprocess.Popen([BIN/'cofleet-tray', '--data-dir', data], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        item = next(iter(wait_for(lambda: items() - before)))
        service, path = item.split('/', 1)
        path = '/' + path
        menu = call(service, path, 'org.freedesktop.DBus.Properties.Get', 'org.kde.StatusNotifierItem', 'Menu')
        menu = re.search(r"objectpath '([^']+)'", menu).group(1)
        def layout():
            return call(service, menu, 'com.canonical.dbusmenu.GetLayout', 0, -1, '[]')
        def click(label):
            text = layout()
            match = re.search(r"\(uint32 (\d+), \{'label': <'" + re.escape(label) + "'>", text)
            if not match:
                match = re.search(r"\((\d+), \{'label': <'" + re.escape(label) + "'>", text)
            assert match, text
            call(service, menu, 'com.canonical.dbusmenu.Event', match.group(1), 'clicked', '<0>', 0)
        # Let the first Agent status enable the pause item.
        wait_for(lambda: '微服未连接' in layout())
        click('暂停接收新任务')
        wait_for(lambda: not json.loads(ctl('status'))['accepting_tasks'])
        click('恢复接收新任务')
        wait_for(lambda: json.loads(ctl('status'))['accepting_tasks'])
        click('退出托盘（后台继续运行）')
        assert tray.wait(timeout=5) == 0
        assert agent.poll() is None
        assert json.loads(ctl('status'))['accepting_tasks']
        print('PASS: tray registration, pause, resume, quit; Agent remains running')
    finally:
        for p in (tray, agent):
            if p is not None and p.poll() is None:
                p.terminate()
                p.wait(timeout=5)
