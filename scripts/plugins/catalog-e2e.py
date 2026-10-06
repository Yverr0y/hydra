#!/usr/bin/env python3
"""Exercise real CLI catalog installs/updates and serve an isolated GUI fixture."""
import argparse
import hashlib
import http.server
import json
import os
import plistlib
import shutil
import sys
from pathlib import Path
import subprocess
import tempfile
import threading
import zipfile

ROOT = Path(__file__).resolve().parents[2]
PLUGIN_ID = 'example.catalog-e2e'


def leb(value):
    result = bytearray()
    while True:
        byte = value & 127
        value >>= 7
        result.append(byte | (128 if value else 0))
        if not value:
            return bytes(result)


def vector(items):
    return leb(len(items)) + b''.join(items)


def wasm():
    def section(number, body):
        return bytes([number]) + leb(len(body)) + body
    def export(name, kind, index):
        data = name.encode()
        return leb(len(data)) + data + bytes([kind, index])
    types = vector([b'\x60\x00\x01\x7f', b'\x60\x01\x7f\x01\x7f',
                    b'\x60\x04\x7f\x7f\x7f\x7f\x01\x7e'])
    bodies = [b'\x00\x41\x01\x0b', b'\x00\x41\x00\x0b', b'\x00\x42\x00\x0b']
    return (b'\x00asm\x01\x00\x00\x00' + section(1, types) +
            section(3, vector([b'\x00', b'\x01', b'\x02'])) +
            section(5, vector([b'\x00\x01'])) +
            section(7, vector([export('memory', 2, 0), export('hydra_api', 0, 0),
                               export('hydra_alloc', 0, 1), export('hydra_call', 0, 2)])) +
            section(10, vector([leb(len(body)) + body for body in bodies])))


def package(root, version):
    manifest = (f'id = "{PLUGIN_ID}"\nname = "Catalog E2E Plugin"\n'
                f'author = "Hydra test fixture"\nversion = "{version}"\n'
                'api = 1\nmodule = "plugin.wasm"\n'
                'welcome = "This is an isolated catalog test plugin."\n'
                '[[settings]]\nkey = "quality"\nlabel = "Quality"\n'
                'type = "text"\ndefault = "best"\n').encode()
    entries = {'hydra-plugin.toml': manifest, 'plugin.wasm': wasm()}
    entries['SHA256SUMS'] = ''.join(
        f'{hashlib.sha256(data).hexdigest()}  {name}\n'
        for name, data in sorted(entries.items())).encode()
    path = root / f'plugin-{version}.hyaplugin'
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as archive:
        for name, data in sorted(entries.items()):
            archive.writestr(name, data)
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--skip-build', action='store_true')
    parser.add_argument('--cli', type=Path, default=ROOT / 'target/debug/hydra')
    parser.add_argument('--gui', action='store_true', help='Launch the GUI with a fresh profile after CLI checks')
    parser.add_argument('--gui-binary', type=Path, default=ROOT / 'target/debug/hydra-gui')
    args = parser.parse_args()
    if not args.skip_build:
        command = ['cargo', 'build', '-p', 'hya-cli']
        if args.gui:
            command += ['-p', 'hya-gui']
        subprocess.run(command, cwd=ROOT, check=True)
    with tempfile.TemporaryDirectory(prefix='hydra-catalog-e2e-') as directory:
        root = Path(directory)
        releases = {version: package(root, version) for version in ('1.0.0', '1.1.0')}
        requests = []
        class Origin(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *values, **kwargs):
                super().__init__(*values, directory=str(root), **kwargs)
            def do_GET(self):
                requests.append(self.path)
                super().do_GET()
            def do_POST(self):
                if self.path != '/publish/1.1.0':
                    self.send_error(404)
                    return
                publish('1.1.0')
                self.send_response(204)
                self.end_headers()
            def log_message(self, *_):
                pass
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Origin)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        origin = f'http://127.0.0.1:{server.server_port}'
        catalog_url = f'{origin}/plugins.json'
        def entry(version):
            path = releases[version]
            return {'id': PLUGIN_ID, 'name': 'Catalog E2E Plugin', 'version': version,
                    'description': 'Local catalog test fixture.', 'is_official': False,
                    'author': 'Hydra test fixture', 'image': f'plugins/img/{PLUGIN_ID}/icon.svg',
                    'homepage': 'https://github.com/ja7ad/hydra', 'api': 1,
                    'download': f'{origin}/{path.name}',
                    'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
        def publish(version):
            data = {'schema': 1, 'name': 'Local E2E Catalog', 'plugins': [entry(version)]}
            pending = root / 'pending.json'
            pending.write_text(json.dumps(data))
            pending.replace(root / 'plugins.json')
        profile = root / 'cli-profile'
        def run(*command, success=True, debug=True, profile_dir=None):
            arguments = [str(args.cli)]
            if debug:
                arguments += ['--debug-plugin-catalog', catalog_url]
            result = subprocess.run(arguments + ['plugin', *command],
                                    env={**os.environ, 'HYDRA_CONFIG_DIR': str(profile_dir or profile)},
                                    capture_output=True, text=True, timeout=30)
            assert result.returncode == 0 if success else result.returncode != 0, (result.stdout, result.stderr)
            return result.stdout
        gui = None
        log = None
        try:
            publish('1.0.0')
            assert json.loads(run('index', 'list'))[0]['url'] == catalog_url
            run('index', 'rm', catalog_url, success=False)
            run('index', 'add', catalog_url, success=False)
            run('install', entry('1.0.0')['download'], '--accept-permissions', debug=False, success=False)
            run('install', entry('1.0.0')['download'], success=False)
            run('install', entry('1.0.0')['download'], '--accept-permissions')
            assert json.loads(run('info', PLUGIN_ID, '--json'))['manifest']['version'] == '1.0.0'
            run('set', PLUGIN_ID, 'quality', 'custom')
            publish('1.1.0')
            mirror = entry('1.1.0')
            mirror['version'] = '9.0.0'
            (root / 'mirror.json').write_text(json.dumps({'schema': 1, 'name': 'Mirror', 'plugins': [mirror]}))
            run('index', 'add', f'{origin}/mirror.json')
            updates = json.loads(run('update'))
            assert len(updates) == 1 and updates[0]['version'] == '1.1.0', updates
            assert json.loads(run('info', PLUGIN_ID, '--json'))['manifest']['version'] == '1.0.0'
            catalog = json.loads((root / 'plugins.json').read_text())
            catalog['plugins'][0]['sha256'] = '0' * 64
            (root / 'plugins.json').write_text(json.dumps(catalog))
            run('update', PLUGIN_ID, '--accept-permissions', success=False)
            assert json.loads(run('info', PLUGIN_ID, '--json'))['manifest']['version'] == '1.0.0'
            publish('1.1.0')
            run('update', PLUGIN_ID, '--accept-permissions')
            installed = json.loads(run('info', PLUGIN_ID, '--json'))
            assert installed['manifest']['version'] == '1.1.0'
            assert installed['settings']['quality'] == 'custom'
            assert installed['previous']['manifest']['version'] == '1.0.0'
            assert json.loads(run('update')) == []
            run('rollback', PLUGIN_ID)
            assert json.loads(run('info', PLUGIN_ID, '--json'))['manifest']['version'] == '1.0.0'
            assert '/plugins.json' in requests and '/plugin-1.1.0.hyaplugin' in requests
            print('PASS: real CLI HTTP install, read-only default, mirror priority, explicit consent, bad checksum refusal, update, preserved settings and rollback', flush=True)
            if args.gui:
                publish('1.0.0')
                gui_profile = root / 'gui-profile'
                gui_profile.mkdir()
                (gui_profile / 'config.toml').write_text('[settings]\ncheck_updates_on_startup = false\n')
                log = (root / 'gui-process.log').open('w')
                executable = args.gui_binary
                application = None
                if sys.platform == 'darwin':
                    application = root / 'Hydra Catalog E2E.app'
                    macos = application / 'Contents/MacOS'
                    macos.mkdir(parents=True)
                    executable = macos / 'hydra-gui'
                    shutil.copy2(args.gui_binary, executable)
                    (application / 'Contents/Info.plist').write_bytes(plistlib.dumps({
                        'CFBundleExecutable': 'hydra-gui',
                        'CFBundleIdentifier': 'io.github.ja7ad.hydra.catalog-e2e',
                        'CFBundleName': 'Hydra Catalog E2E',
                        'CFBundlePackageType': 'APPL',
                        'NSHighResolutionCapable': True,
                    }))
                gui = subprocess.Popen([str(executable), '--config', str(gui_profile),
                                        '--debug-plugin-catalog', catalog_url], stdout=log, stderr=log)
                print(json.dumps({'catalog': catalog_url, 'install': entry('1.0.0')['download'],
                                  'publish_update': f'{origin}/publish/1.1.0',
                                  'profile': str(gui_profile), 'application': str(application) if application else None, 'pid': gui.pid}), flush=True)
                print('GUI fixture running: install version 1.0.0 in Plugins, POST publish_update, then Check plugin updates and use the row install icon. Ctrl+C cleans up.', flush=True)
                gui.wait()
        finally:
            if gui is not None and gui.poll() is None:
                gui.terminate()
                gui.wait(timeout=10)
            if log is not None:
                log.close()
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


if __name__ == '__main__':
    main()
