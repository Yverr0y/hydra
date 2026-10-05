#!/usr/bin/env python3
"""Drive real terminal plugin install, settings and enable/disable through a PTY."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time

root = Path(__file__).resolve().parents[2]
if not os.environ.get('HYDRA_E2E_SKIP_BUILD'):
    subprocess.run(['cargo','build','-p','hya-cli'],cwd=root,check=True)
    subprocess.run(['cargo','build','--manifest-path','examples/plugins/direct/Cargo.toml','--release','--target','wasm32-wasip1'],cwd=root,check=True)
cli = Path(os.environ.get('HYDRA_E2E_CLI', root / 'target/debug/hydra'))
with tempfile.TemporaryDirectory(prefix='hydra-tui-plugin-e2e-') as directory:
    work = Path(directory)
    env = {**os.environ, 'HYDRA_CONFIG_DIR': str(work/'profile'), 'TERM': 'xterm-256color'}
    package = work/'package'
    package.mkdir()
    module = root/'examples/plugins/direct/target/wasm32-wasip1/release/hydra_example_direct.wasm'
    (package/'plugin.wasm').write_bytes(module.read_bytes())
    manifest = '''id="example.direct"
name="Terminal E2E plugin"
version="0.1.0"
api=1
module="plugin.wasm"
hooks=["resolve","check"]
claims=["https://example.com/*"]
[permissions]
sources=["example.com"]
'''
    for key, kind, default, options in [
        ('prefix','textbox','"Default"',''),
        ('quality','dropdown','"small"','options=["small","large"]'),
        ('count','number','1',''),
        ('container','radio','"mp4"','options=["mp4","mkv"]'),
        ('captions','checkbox','false',''),
        ('enabled','boolean','false',''),
        ('token','secret','""',''),
    ]:
        manifest += f'\n[[settings]]\nkey="{key}"\nlabel="{key}"\ntype="{kind}"\ndefault={default}\n{options}\n'
    (package/'hydra-plugin.toml').write_text(manifest)
    pid, terminal = pty.fork()
    if pid == 0:
        os.chdir(work)
        os.execve(cli, [str(cli),'interactive','--queue-file',str(work/'queue.json')], env)
    fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack('HHHH',40,120,0,0))
    transcript = bytearray()
    def wait_for(text, timeout=15):
        seen = bytearray()
        end = time.monotonic()+timeout
        while time.monotonic() < end:
            if select.select([terminal],[],[],0.2)[0]:
                chunk = os.read(terminal,65536)
                transcript.extend(chunk)
                seen.extend(chunk)
                if text.encode() in seen:
                    return
        raise AssertionError(f'terminal did not show {text!r}: {seen[-2000:]!r}')
    def send(text):
        os.write(terminal,text.encode())
    try:
        wait_for('file retriever')
        send('P');wait_for('Plugins')
        send('i');wait_for('Install path:')
        send(str(package)+'\r');wait_for('y accept and install')
        send('y');wait_for('[x] Terminal E2E plugin')
        send('e');wait_for('[ ] Terminal E2E plugin')
        send('e');wait_for('[x] Terminal E2E plugin')
        send('s');wait_for('Settings')
        send('custom\tlarge\t3\tmkv\ttrue\ttrue\tpr1vate\r')
        wait_for('file retriever')
        info=json.loads(subprocess.check_output([cli,'plugin','info','example.direct','--json'],env=env))
        assert info['settings'] == {'prefix':'custom','quality':'large','count':3.0,'container':'mkv','captions':True,'enabled':True}, info['settings']
        assert 'pr1vate' not in transcript.decode(errors='replace')
        assert 'pr1vate' not in (work/'profile/plugins/state.toml').read_text()
        send('P');wait_for('Terminal E2E plugin')
        send('g');wait_for('Capability')
        send('-sources:example.com\r');wait_for('e enable/disable')
        info=json.loads(subprocess.check_output([cli,'plugin','info','example.direct','--json'],env=env))
        assert not info['grants']['sources']
        send('g');wait_for('Capability')
        send('+sources:example.com\r');wait_for('e enable/disable')
        info=json.loads(subprocess.check_output([cli,'plugin','info','example.direct','--json'],env=env))
        assert info['grants']['sources'] == ['example.com']
        send('\x1b');wait_for('file retriever');send('q')
        deadline=time.monotonic()+15
        while True:
            ended,status=os.waitpid(pid,os.WNOHANG)
            if ended:
                assert os.waitstatus_to_exitcode(status)==0
                pid=None
                break
            if time.monotonic()>deadline:
                raise AssertionError('terminal did not exit after q')
            if select.select([terminal],[],[],0.1)[0]:
                try: transcript.extend(os.read(terminal,65536))
                except OSError: pass
    finally:
        os.close(terminal)
        if pid is not None:
            os.kill(pid,signal.SIGTERM)
            os.waitpid(pid,0)
print('PASS: real TUI install consent, all settings types, secret masking, enable/disable and grants')
