#!/usr/bin/env python3
"""Exercise authoring, plugin downloads, and the terminal UI with real binaries."""
import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument("--hydra", type=Path, default=Path("target/debug/hydra"))
parser.add_argument("--authoring", type=Path, default=Path("target/debug/hydra-plugin"))
args = parser.parse_args()
hydra = args.hydra.resolve()
authoring = args.authoring.resolve()
payload = b"Hydra plugin end-to-end payload\n"

class Handler(http.server.BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()

    def do_GET(self):
        self.do_HEAD()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass

with tempfile.TemporaryDirectory(prefix="hydra-plugin-e2e-") as directory:
    root = Path(directory)
    env = {**os.environ, "HYDRA_CONFIG_DIR": str(root / "profile"), "TERM": "xterm-256color"}

    def run(binary, *arguments, success=True):
        result = subprocess.run([str(binary), *arguments], cwd=root, env=env,
                                capture_output=True, text=True, timeout=180)
        assert (result.returncode == 0) == success, (arguments, result.stdout, result.stderr)
        return result.stdout

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    process = None
    master = None
    transcript = bytearray()

    def expect(value):
        start = len(transcript)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            readable, _, _ = select.select([master], [], [], 0.2)
            if readable:
                data = os.read(master, 65536)
                transcript.extend(data)
                if b"\x1b[6n" in data:
                    os.write(master, b"\x1b[1;1R")
                if value.encode() in transcript[start:]:
                    return
            assert process.poll() is None, transcript.decode(errors="replace")
        raise AssertionError(f"Missing {value!r}: {transcript.decode(errors='replace')}")

    try:
        master, slave = pty.openpty()
        process = subprocess.Popen([str(authoring), "init"], cwd=root, env=env,
                                   stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        for label, answer in [("Name [", "Youtube test"), ("Plugin ID", "tester.wizard"),
                              ("Directory path", "wizard-project"), ("Version", "1.2.3"),
                              ("Author name", "Wizard Author"), ("Language (", "rust")]:
            expect(label)
            os.write(master, (answer + "\n").encode())
        expect("Next: hydra-plugin build")
        assert process.wait(timeout=15) == 0
        wizard_manifest = (root / "wizard-project/hydra-plugin.toml").read_text()
        for value in ["Youtube test", "tester.wizard", "1.2.3", "Wizard Author"]:
            assert value in wizard_manifest, wizard_manifest
        os.close(master)
        master = None
        transcript.clear()
        run(authoring, "init", "sample")
        project = root / "sample"
        manifest = project / "hydra-plugin.toml"
        manifest.write_text(manifest.read_text().replace('sources = ["example.com"]',
                            'sources = ["127.0.0.1"]').replace('name = "sample"',
                            'name = "E2E resolver"\nauthor = "E2E Author"'))
        source = project / "src/lib.rs"
        source.write_text(source.read_text().replace('req.url',
                          f'"http://127.0.0.1:{server.server_port}/payload"')
                          .replace('req: ResolveRequest', '_req: ResolveRequest'))
        run(authoring, "build", "sample")
        run(authoring, "pack", "sample", "--output", "sample.hyaplugin")
        run(authoring, "validate", "sample.hyaplugin")
        run(hydra, "plugin", "install", "sample.hyaplugin", success=False)
        run(hydra, "plugin", "install", "sample.hyaplugin", "--accept-permissions")
        listing = run(hydra, "plugin", "list")
        for value in ["NAME", "ID", "AUTHOR", "VERSION", "E2E resolver", "example.sample", "E2E Author", "0.1.0"]:
            assert value in listing, listing
        run(hydra, "plugin", "check", "example.sample")
        run(hydra, "plugin", "resolve", "https://example.com/file")
        run(hydra, "--no-proxy", "-O", "download.bin", "https://example.com/file")
        assert (root / "download.bin").read_bytes() == payload
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        process = subprocess.Popen([str(hydra), "interactive", "--queue-file", str(root / "queue.json")],
                                   cwd=root, env=env, stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        expect("plugins")
        os.write(master, b"P")
        expect("E2E resolver | example.sample | E2E Author | 0.1.0")
        for enabled in [False, True]:
            os.write(master, b"e")
            expect("[x] E2E resolver" if enabled else "[ ] E2E resolver")
            installed = json.loads(run(hydra, "plugin", "list", "--json"))
            assert installed[0]["enabled"] is enabled, installed
        os.write(master, b"\x1b")
        expect("plugins")
        os.write(master, b"q")
        assert process.wait(timeout=15) == 0
        run(hydra, "plugin", "remove", "example.sample")
        assert run(hydra, "plugin", "list").strip() == "No plugins installed."
        print("PASS: interactive init with author, init, build, pack, validate, consent, list, check, resolve, download, TUI metadata/toggle/quit, remove")
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait()
        if master is not None:
            os.close(master)
        server.shutdown()
        server.server_close()
