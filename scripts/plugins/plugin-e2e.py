#!/usr/bin/env python3
"""Hermetic CLI plugin install/use/settings/permission lifecycle with a real Wasm guest."""
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

root = Path(__file__).resolve().parents[2]
if not os.environ.get('HYDRA_E2E_SKIP_BUILD'):
    subprocess.run(['cargo', 'build', '-p', 'hya-cli', '-p', 'hya-plugin-cli'], cwd=root, check=True)
    subprocess.run(['cargo', 'build', '--manifest-path', 'examples/plugins/direct/Cargo.toml',
                    '--release', '--target', 'wasm32-wasip1'], cwd=root, check=True)
body = bytes(range(251)) * 1001
class Origin(http.server.BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Content-Type', 'application/octet-stream')
        self.end_headers()
    def do_GET(self):
        self.do_HEAD()
        self.wfile.write(body)
    def log_message(self, *_): pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Origin)
threading.Thread(target=server.serve_forever, daemon=True).start()
cli = Path(os.environ.get('HYDRA_E2E_CLI', root / 'target/debug' / ('hydra.exe' if os.name == 'nt' else 'hydra')))
author = root / 'target/debug' / ('hydra-plugin.exe' if os.name == 'nt' else 'hydra-plugin')
with tempfile.TemporaryDirectory(prefix='hydra-cli-plugin-e2e-') as directory:
    work = Path(directory)
    env = {**os.environ, 'HYDRA_CONFIG_DIR': str(work / 'profile')}
    package = work / 'package'
    package.mkdir()
    wasm = root / 'examples/plugins/direct/target/wasm32-wasip1/release/hydra_plugin_direct.wasm'
    # The Cargo package name determines the artifact's filename.
    import tomllib
    cargo = tomllib.loads((root/'examples/plugins/direct/Cargo.toml').read_text())
    wasm = wasm.with_name(cargo['package']['name'].replace('-', '_') + '.wasm')
    (package/'plugin.wasm').write_bytes(wasm.read_bytes())
    (package/'hydra-plugin.toml').write_text('''id = "example.direct"
name = "CLI E2E resolver"
version = "0.1.0"
api = 1
module = "plugin.wasm"
hooks = ["resolve", "check", "refresh"]
claims = ["http://127.0.0.1/*"]
[permissions]
sources = ["127.0.0.1"]
[[settings]]
key = "enabled"
label = "Enabled setting"
type = "boolean"
default = false
''')
    archive = work/'direct.hyaplugin'
    def run(*args, ok=True):
        result = subprocess.run([str(cli), *map(str,args)], env=env, cwd=work, capture_output=True, text=True, timeout=60)
        assert (result.returncode == 0) == ok, (args, result.stdout, result.stderr)
        return result.stdout
    run('plugin', 'pack', package, archive)
    subprocess.run([str(author), 'validate', str(archive)], check=True)
    run('plugin', 'install', archive, ok=False)
    assert json.loads(run('plugin', 'list', '--json')) == []
    run('plugin', 'install', archive, '--accept-permissions')
    run('plugin', 'set', 'example.direct', 'enabled', 'true')
    assert json.loads(run('plugin','info','example.direct','--json'))['settings']['enabled'] is True
    url = f'http://127.0.0.1:{server.server_port}/payload'
    assert json.loads(run('plugin','resolve',url))[0] == 'example.direct'
    output = work/'payload.bin'
    run('--plugin','example.direct','--no-proxy','-O',output,url)
    assert hashlib.sha256(output.read_bytes()).digest() == hashlib.sha256(body).digest()
    run('plugin','revoke','example.direct','sources:127.0.0.1')
    run('plugin','resolve',url,ok=False)
    run('plugin','grant','example.direct','sources:127.0.0.1')
    run('plugin','disable','example.direct')
    run('--plugin','example.direct','--list-tracks',url,ok=False)
    run('plugin','enable','example.direct')
    assert json.loads(run('plugin','resolve',url))[0] == 'example.direct'
    run('plugin','remove','example.direct')
    assert json.loads(run('plugin','list','--json')) == []
server.shutdown()
print('PASS: CLI plugin install, consent, settings, resolve, download hash, grants, enable/disable and removal')
