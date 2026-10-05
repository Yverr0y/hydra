#!/usr/bin/env python3
"""Build and validate an official .hyaplugin with the shared authoring tool."""
from pathlib import Path
import subprocess
import sys
import tomllib

root = Path(__file__).resolve().parent
if len(sys.argv) != 2 or Path(sys.argv[1]).name != sys.argv[1]:
    raise SystemExit("usage: python3 plugins/build.py PLUGIN_FOLDER")
plugin = root / sys.argv[1]
cargo = tomllib.loads((plugin / "Cargo.toml").read_text())
name = cargo["package"].get("metadata", {}).get("hydra", {}).get("package-name", cargo["package"]["name"])
subprocess.run(["cargo", "run", "--manifest-path", str(root.parent / "Cargo.toml"),
                "-p", "hya-plugin-cli", "--", "build", str(plugin),
                "--output", str(plugin / (name + ".hyaplugin"))], check=True)
