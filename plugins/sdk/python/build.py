#!/usr/bin/env python3
"""Embed Python source and its interpreter in a standalone Wasm module."""
import os
from pathlib import Path
import shutil
import subprocess
import sys

if len(sys.argv) not in (2, 3):
    raise SystemExit("usage: python3 build.py plugin.py [plugin.wasm]")
runtime = Path(__file__).resolve().parent.parent / "runtime"
subprocess.run(["cargo", "build", "--locked", "--manifest-path", str(runtime / "Cargo.toml"),
                "--target-dir", str(runtime / "target"), "--release", "--target", "wasm32-wasip1", "--features", "python"],
               env={**os.environ, "HYDRA_PLUGIN_SOURCE": str(Path(sys.argv[1]).resolve())}, check=True)
shutil.copyfile(runtime / "target/wasm32-wasip1/release/hydra_script_guest.wasm",
                sys.argv[2] if len(sys.argv) == 3 else "plugin.wasm")
