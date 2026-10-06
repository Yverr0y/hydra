"""End-to-end CLI downloads through the installed Wasm/native torrent package."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import native

CLI = Path(sys.argv[1]).resolve()
PACKAGE = Path(sys.argv[2]).resolve()

with tempfile.TemporaryDirectory(prefix="hydra-torrent-cli-") as temporary:
    root = Path(temporary)
    environment = dict(os.environ, HYDRA_CONFIG_DIR=str(root / "profile"))
    installed = subprocess.run([str(CLI), "plugin", "install", str(PACKAGE), "--accept-permissions"], env=environment, capture_output=True, text=True, timeout=30)
    if installed.returncode:
        raise RuntimeError(installed.stderr + installed.stdout)
    swarm = native.Swarm(root, native.lt.create_torrent.v1_only, private=False)
    try:
        torrent = root / "input with spaces.torrent"
        torrent.write_bytes(swarm.metainfo)
        info = native.lt.torrent_info(native.lt.bdecode(swarm.metainfo))
        magnet = native.lt.make_magnet_uri(info) + f"&x.pe=127.0.0.1:{swarm.port}"
        for label, address in [("file", str(torrent)), ("magnet", magnet)]:
            destination = root / label
            result = subprocess.run([str(CLI), address, "--output-dir", str(destination), "--no-proxy", "--quiet"], env=environment, capture_output=True, text=True, timeout=30)
            if result.returncode:
                raise RuntimeError(result.stdout + result.stderr)
            for name, content in swarm.contents.items():
                output = destination / "bundle" / name.removeprefix("bundle/")
                if output.read_bytes() != content:
                    raise AssertionError(f"{label}: bytes differ for {name}")
            for flag, value, expected in [("--no-clobber", None, "no-clobber"), ("--max-filesize", "1", "max-filesize")]:
                arguments = [str(CLI), address, "--output-dir", str(destination), "--no-proxy", "--quiet", flag]
                if value is not None: arguments.append(value)
                refused = subprocess.run(arguments, env=environment, capture_output=True, text=True, timeout=30)
                if refused.returncode == 0 or expected not in refused.stderr + refused.stdout:
                    raise AssertionError(f"{label}: {flag} was not enforced: {refused.stderr}")
            print(f"CLI {label}: downloaded and verified every file; limits and no-clobber enforced")
    finally:
        swarm.close()
