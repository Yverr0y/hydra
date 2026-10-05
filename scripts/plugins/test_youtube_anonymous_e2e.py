#!/usr/bin/env python3
"""Check that the live test cannot report success without downloaded media."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location('youtube_e2e', Path(__file__).with_name('youtube-anonymous-e2e.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class LiveReportTests(unittest.TestCase):
    def exercise(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)

            def run(command, **kwargs):
                if command[0] == 'ffprobe':
                    if mode == 'invalid-media':
                        return subprocess.CompletedProcess(command, 1, '', 'invalid media')
                    return subprocess.CompletedProcess(command, 0, json.dumps({'streams': [{'codec_type': 'audio'}]}), '')
                if '--version' in command:
                    return subprocess.CompletedProcess(command, 0, 'test-version\n', '')
                if '--flat-playlist' in command:
                    entries = [{'id': str(index)} for index in range(3)]
                    return subprocess.CompletedProcess(command, 0, json.dumps({'entries': entries}), '')
                if mode == 'timeout':
                    raise subprocess.TimeoutExpired(command, 150, output=b'', stderr=b'network stalled')
                if mode == 'blocked':
                    return subprocess.CompletedProcess(command, 1, '', "ERROR: Sign in to confirm you're not a bot")
                if mode != 'empty':
                    output = Path(command[command.index('-o') + 1]).parent / 'media.wav'
                    output.write_bytes(b'media fixture')
                stderr = 'PO Token Providers: bgutil:http; Retrieved a player PO Token' if '--plugin-dirs' in command else ''
                if mode == 'unloaded':
                    stderr = ''
                return subprocess.CompletedProcess(command, 0, '', stderr)

            arguments = ['e2e', '--python', 'test-python', '--plugin-dir', str(work), '--proxy', 'http://test-proxy:8080']
            with patch.object(module.subprocess, 'run', side_effect=run), \
                    patch.object(module.tempfile, 'mkdtemp', return_value=directory), \
                    patch.object(module.shutil, 'which', return_value='/test/tool'), \
                    patch('sys.argv', arguments), contextlib.redirect_stdout(io.StringIO()):
                status = module.main()
            return status, json.loads((work / 'report.json').read_text())

    def test_requires_media_and_loaded_provider_for_success(self):
        status, report = self.exercise('downloaded')
        self.assertEqual(status, 0)
        self.assertTrue(report['passed'])
        self.assertTrue(all(item['verified_media'] for item in report['results'][1:]))

    def test_metadata_success_without_downloaded_files_is_failure(self):
        status, report = self.exercise('empty')
        self.assertEqual(status, 1)
        self.assertFalse(report['passed'])

    def test_missing_provider_cannot_be_mistaken_for_provider_success(self):
        status, report = self.exercise('unloaded')
        self.assertEqual(status, 1)
        self.assertFalse(report['passed'])

    def test_server_rejection_is_recorded_as_failure(self):
        status, report = self.exercise('blocked')
        self.assertEqual(status, 1)
        self.assertTrue(all(item['bot_rejection'] for item in report['results'][1:]))

    def test_network_timeout_produces_a_failed_report(self):
        status, report = self.exercise('timeout')
        self.assertEqual(status, 1)
        self.assertTrue(all(item['exit_code'] == 124 for item in report['results'][1:]))

    def test_corrupt_download_is_not_success(self):
        status, report = self.exercise('invalid-media')
        self.assertEqual(status, 1)
        self.assertFalse(any(item['verified_media'] for item in report['results'][1:]))

    def test_missing_tools_and_invalid_sample_limits_stop_before_network(self):
        base = ['e2e', '--python', 'test-python', '--plugin-dir', '/tmp']
        for samples, available in [(3, None), (0, '/tool'), (11, '/tool')]:
            with patch('sys.argv', [*base, '--samples', str(samples)]), \
                    patch.object(module.shutil, 'which', return_value=available), \
                    patch.object(module.subprocess, 'run') as run, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                module.main()
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
