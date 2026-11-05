#!/usr/bin/env python3
"""Isolated user-service lifecycle; fake systemctl never touches the current user's manager."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

binary = Path('target/debug/cofleetctl').resolve()
with tempfile.TemporaryDirectory(prefix='cofleet-user-service-') as temporary:
    root = Path(temporary)
    bins = root / 'bin'
    bins.mkdir()
    env = dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(root/'config'), XDG_DATA_HOME=str(root/'share'), COFLEET_TOKEN='fixture-development-token-0123456789')
    env['PATH'] = str(bins) + os.pathsep + env['PATH']
    (bins/'systemctl').write_text('#!/bin/sh\ncase "$*" in *restart*) exit 1;; esac\nexit 0\n')
    (bins/'systemctl').chmod(0o700)
    node = root / 'node path % $literal'
    def ctl(*args, ok=True):
        result = subprocess.run([str(binary), '--data-dir', str(node), *args],env=env,capture_output=True,text=True,timeout=15)
        assert (result.returncode == 0) == ok, result.stderr
        return result
    ctl('init','--name','fixture','--manager','http://127.0.0.1:1')
    original = (node/'agent.toml').read_bytes()
    source = root/'original-agent'
    source.write_text('#!/bin/sh\nexit 0\n');source.chmod(0o700)
    options = ['user-service','install','--agent',str(source),'--no-start','--docker','--docker-observe','--workspace-image','sha256:'+'a'*64,'--build-deploy']
    ctl(*options)
    unit = root/'config/systemd/user/cofleet-agent.service'
    contents = unit.read_text()
    assert '--docker-observe' in contents and '--build-deploy' in contents
    assert '%% $$literal' in contents and 'User=root' not in contents
    spec = root/'share/cofleet/service/service.json'
    installed = root/'share/cofleet/service/cofleet-agent'
    old = installed.read_bytes()
    ctl(*options)  # Exact reinstall is idempotent.
    ctl('user-service','install','--agent',str(source),'--no-start',ok=False)  # No silent capability loss.
    source.write_text('#!/bin/sh\nexit 1\n')
    ctl('user-service','upgrade','--agent',str(source),ok=False)
    assert installed.read_bytes() == old and unit.read_text() == contents
    assert json.loads(spec.read_text())['docker_observe']
    ctl('user-service','uninstall')
    assert not unit.exists() and not spec.exists()
    assert (node/'agent.toml').read_bytes() == original
    print('PASS literal service paths, capability preservation, idempotent install, failed upgrade rollback, uninstall preserves node identity')
