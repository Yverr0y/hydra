#!/usr/bin/env python3
"""Test real anonymous YouTube listing, Wasm resolution and media transfer.

Requires yt-dlp and Deno. Network rejection is a failing test, never a pass.
The isolated profile does not import browser cookies or user yt-dlp config.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--playlist', default='https://www.youtube.com/playlist?list=PLDKUP6Nm13I6RNDlyggSRkXmjqyMUI8yQ')
    parser.add_argument('--video', default='https://www.youtube.com/watch?v=9KkbfDVQYO0')
    parser.add_argument('--skip-build', action='store_true')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    cli = Path(os.environ.get('HYDRA_E2E_CLI', root / 'target/debug/hydra')).resolve()
    if not args.skip_build:
        subprocess.run(['cargo', 'build', '-p', 'hya-cli'], cwd=root, check=True)
        subprocess.run(['cargo', 'build', '--manifest-path', 'plugins/hydra-youtube/Cargo.toml', '--release', '--target', 'wasm32-wasip1'], cwd=root, check=True)
    backend = shutil.which('yt-dlp')
    if not backend or not shutil.which('deno'):
        parser.error('yt-dlp and deno must be installed')
    work = Path(tempfile.mkdtemp(prefix='hydra-youtube-live-'))
    env = {**os.environ, 'HYDRA_CONFIG_DIR': str(work / 'profile')}
    results = []

    def run(name, command, required=False):
        result = subprocess.run(list(map(str, command)), cwd=work, env=env, capture_output=True, text=True, timeout=180)
        (work / f'{name}.stdout').write_text(result.stdout)
        (work / f'{name}.stderr').write_text(result.stderr)
        if required and result.returncode:
            raise RuntimeError(f'{name} failed; see {work / (name + ".stderr")}')
        return result

    package = work / 'package'
    package.mkdir()
    shutil.copy2(root / 'plugins/hydra-youtube/hydra-plugin.toml', package)
    shutil.copy2(root / 'plugins/hydra-youtube/target/wasm32-wasip1/release/hydra_youtube.wasm', package / 'plugin.wasm')
    archive = work / 'youtube.hyaplugin'
    run('pack', [cli, 'plugin', 'pack', package, archive], True)
    run('install', [cli, 'plugin', 'install', archive, '--accept-permissions'], True)
    run('cookies-off', [cli, 'plugin', 'set', 'hydra.youtube', 'use_cookies', 'false'], True)
    for index, domain in enumerate(['youtube.com', '*.youtube.com']):
        run(f'cookies-denied-{index}', [cli, 'plugin', 'revoke', 'hydra.youtube', f'cookies:{domain}'], True)
    run('runtime', [cli, 'plugin', 'set', 'hydra.youtube', 'javascript_runtime', 'deno'], True)
    common = [backend, '--ignore-config', '--no-plugin-dirs', '--js-runtimes', 'deno']
    for name, command, playlist in [
        ('yt-dlp-playlist', [*common, '--flat-playlist', '--yes-playlist', '--playlist-end', '512', '-J', '--skip-download', '--', args.playlist], True),
        ('hydra-playlist', [cli, '--list-tracks', args.playlist], True),
        ('yt-dlp-video', [*common, '--no-playlist', '-J', '--skip-download', '--', args.video], False),
        ('hydra-video', [cli, '--list-tracks', args.video], False),
        ('hydra-download', [cli, '--no-input', '--extract-audio', '--audio-format', 'mp3', '-O', work / 'download.mp3', args.video], False),
    ]:
        result = run(name, command)
        entry = {'stage': name, 'exit_code': result.returncode}
        if result.returncode == 0 and playlist:
            entry['items'] = len(json.loads(result.stdout).get('entries', []))
            if not entry['items']:
                entry['validation_error'] = 'Playlist returned no entries'
        if name == 'hydra-download' and result.returncode == 0:
            if not (work / 'download.mp3').is_file() or not (work / 'download.mp3').stat().st_size:
                entry['validation_error'] = 'No nonempty downloaded file'
        results.append(entry)
        print(json.dumps(entry), flush=True)
    run('plugin-logs', [cli, 'plugin', 'logs', 'hydra.youtube'])
    passed = all(item['exit_code'] == 0 and 'validation_error' not in item for item in results)
    report = {'passed': passed, 'cookies': False, 'playlist': args.playlist, 'video': args.video, 'results': results, 'artifacts': str(work)}
    (work / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'{"PASS" if passed else "FAIL"}: live anonymous E2E; report: {work / "report.json"}', flush=True)
    return 0 if passed else 1


if __name__ == '__main__':
    raise SystemExit(main())
