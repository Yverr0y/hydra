#!/usr/bin/env python3
"""Stage repository-owned assets inside publishable plugin crates."""
from pathlib import Path
import shutil
import tomllib

root = Path(__file__).resolve().parents[2]
manifest = tomllib.loads((root / "Cargo.toml").read_text())
(root / "crates/hydra-plugin/product-version.txt").write_text(
    manifest["workspace"]["package"]["version"] + "\n"
)
assets = root / "crates/hydra-plugin-cli/assets"
if assets.exists():
    shutil.rmtree(assets)
for directory in ["plugins/sdk", "crates/hydra-plugin-sdk", "crates/hydra-plugin-api"]:
    source = root / directory
    for path in source.rglob("*"):
        if not path.is_file() or any(
            part in {"target", "__pycache__", ".git"} for part in path.relative_to(source).parts
        ):
            continue
        if path.suffix not in {".rs", ".toml", ".lock", ".go", ".mod", ".py", ".js", ".mjs", ".c", ".h", ".md"}:
            continue
        destination = assets / path.relative_to(root)
        if destination.name == "Cargo.toml":
            destination = destination.with_name("Cargo.toml.template")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
