#!/usr/bin/env python3
"""Download real YouTube playlist samples using standalone yt-dlp without cookies.

Requires a Python environment containing yt-dlp and EJS, Deno, ffprobe,
and a running bgutil PO-token server. Does not configure or invoke Hydra.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--python', required=True)
    parser.add_argument('--plugin-dir', type=Path, required=True)
    parser.add_argument('--provider-url', default='http://127.0.0.1:4417')
    parser.add_argument('--proxy', default='', help='Optional proxy for both yt-dlp and token requests')
    parser.add_argument('--playlist', default='https://www.youtube.com/playlist?list=PLDKUP6Nm13I6RNDlyggSRkXmjqyMUI8yQ')
    parser.add_argument('--video', default='https://www.youtube.com/watch?v=9KkbfDVQYO0')
    parser.add_argument('--samples', type=int, default=3)
    args = parser.parse_args()
    if not shutil.which('deno') or not shutil.which('ffprobe'):
        parser.error('deno and ffprobe must be installed')
    if args.samples < 1 or args.samples > 10:
        parser.error('--samples must be between 1 and 10')
    work = Path(tempfile.mkdtemp(prefix='youtube-anonymous-e2e-'))
    common = [args.python, '-m', 'yt_dlp', '--ignore-config', '--no-plugin-dirs',
              '--js-runtimes', 'deno', '--verbose', '--proxy', args.proxy,
              '--socket-timeout', '20', '--retries', '1', '--extractor-retries', '1',
              '--cache-dir', str(work / 'cache')]
    provider = ['--plugin-dirs', str(args.plugin_dir.resolve()),
                '--extractor-args', f'youtubepot-bgutilhttp:base_url={args.provider_url}']
    results = []

    def run(name, command):
        try:
            result = subprocess.run(command, capture_output=True, text=True, timeout=150)
            stdout, stderr, code = result.stdout, result.stderr, result.returncode
        except subprocess.TimeoutExpired as error:
            stdout = (error.stdout or b'').decode(errors='replace')
            stderr = (error.stderr or b'').decode(errors='replace') + '\nE2E timeout after 150 seconds'
            code = 124
        if args.proxy:
            stdout = stdout.replace(args.proxy, '[proxy]')
            stderr = stderr.replace(args.proxy, '[proxy]')
        (work / f'{name}.stdout').write_text(stdout)
        (work / f'{name}.stderr').write_text(stderr)
        return code, stdout, stderr

    version = run('version', [args.python, '-m', 'yt_dlp', '--version'])[1].strip()
    code, stdout, _ = run('playlist', [*common, '--flat-playlist', '--yes-playlist', '--playlist-end', '512', '-J', '--skip-download', '--', args.playlist])
    entries = json.loads(stdout).get('entries', []) if code == 0 else []
    results.append({'stage': 'playlist', 'exit_code': code, 'items': len(entries)})
    print(json.dumps(results[-1]), flush=True)

    def download(name, url, options):
        output = work / name
        output.mkdir()
        code, _, stderr = run(name, [*common, *options, '--no-playlist', '-f', 'worstaudio/worst',
                                    '-o', str(output / '%(id)s.%(ext)s'), '--', url])
        item = {'stage': name, 'exit_code': code,
                'provider_loaded': 'PO Token Providers: bgutil:http' in stderr,
                'token_retrieved': 'Retrieved a player PO Token' in stderr or 'Retrieved a gvs PO Token' in stderr,
                'bot_rejection': 'Sign in to confirm' in stderr and 'not a bot' in stderr,
                'verified_media': False}
        if code == 0:
            files = [p for p in output.iterdir() if p.is_file() and p.suffix not in ('.part', '.ytdl')]
            if len(files) == 1 and files[0].stat().st_size > 0:
                probe_code, probe_text, _ = run(name + '-ffprobe', ['ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(files[0])])
                streams = json.loads(probe_text).get('streams', []) if probe_code == 0 else []
                item['verified_media'] = any(s.get('codec_type') in ('audio', 'video') for s in streams)
                item['bytes'] = files[0].stat().st_size
                item['sha256'] = hashlib.sha256(files[0].read_bytes()).hexdigest()
        results.append(item)
        print(json.dumps(item), flush=True)

    download('baseline-failing-video', args.video, [])
    for client in ['mweb', 'web', 'android_vr']:
        download(f'provider-{client}-failing-video', args.video,
                 [*provider, '--extractor-args', f'youtube:player_client={client};fetch_pot=always'])
    for index, entry in enumerate(entries[:args.samples]):
        url = 'https://www.youtube.com/watch?v=' + entry['id']
        download(f'provider-mweb-playlist-{index + 1}', url,
                 [*provider, '--extractor-args', 'youtube:player_client=mweb;fetch_pot=always'])
    attempted = [item for item in results if item['stage'] != 'playlist']
    provider_attempts = [item for item in attempted if item['stage'].startswith('provider-')]
    target_passed = any(item['verified_media'] for item in attempted if 'failing-video' in item['stage'])
    sample_passed = len(entries) >= args.samples and all(
        item['verified_media'] for item in attempted if '-playlist-' in item['stage'])
    passed = bool(entries) and target_passed and sample_passed and all(item['provider_loaded'] for item in provider_attempts)
    report = {'passed': passed, 'yt_dlp_version': version, 'browser_cookies': False,
              'proxy_enabled': bool(args.proxy), 'results': results, 'artifacts': str(work)}
    (work / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'{"PASS" if passed else "FAIL"}: actual anonymous media downloads; report: {work / "report.json"}')
    return 0 if passed else 1


if __name__ == '__main__':
    raise SystemExit(main())
