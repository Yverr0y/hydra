#!/usr/bin/env python3
"""Build each scaffold and run it through Hydra's actual host ABI."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('languages', nargs='*', default=['rust', 'c', 'go', 'nodejs', 'python'])
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
subprocess.run(['cargo', 'build', '-p', 'hya-plugin-cli'], cwd=root, check=True)
cli = root / 'target/debug' / ('hydra-plugin.exe' if os.name == 'nt' else 'hydra-plugin')
with tempfile.TemporaryDirectory(prefix='hydra-sdk-check-') as directory:
    for language in args.languages:
        project = Path(directory) / language
        for command in ([str(cli), 'init', str(project), '--language', language, '--name', 'sample'],
                        [str(cli), 'build', str(project)],
                        [str(cli), 'validate', str(project / 'sample.hyaplugin')]):
            subprocess.run(command, cwd=root, check=True)
        subprocess.run(['cargo', 'test', '-p', 'hya-plugin', '--test', 'sdk'], cwd=root,
                       env={**os.environ, 'HYDRA_TEST_PLUGIN_WASM': str(project / 'plugin.wasm')}, check=True)
        print('PASS', language, flush=True)
