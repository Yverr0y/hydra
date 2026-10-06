#!/usr/bin/env python3
"""Stage signed portable and platform-specific plugins for application packaging."""
import argparse
import io
import pathlib
import shutil
import tempfile
import urllib.request
import zipfile


def stage(root, version):
    destination = root / "plugins/bundled"
    marker = destination / "bundle-version.txt"
    if (marker.is_file() and marker.read_text().strip() == version
            and any(destination.glob("*.hyaplugin"))):
        return destination
    address = f"https://github.com/ja7ad/hydra/releases/download/v{version}/hydra-official-plugins.zip"
    with urllib.request.urlopen(address, timeout=60) as response:
        data = response.read(64 * 1024 * 1024 + 1)
    if len(data) > 64 * 1024 * 1024:
        raise ValueError("official bundle exceeds 64 MiB")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        staging = pathlib.Path(temporary) / "bundled"
        staging.mkdir()
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            names = set()
            total = 0
            for entry in archive.infolist():
                parts = pathlib.PurePosixPath(entry.filename).parts
                platforms = {f"{system}-{architecture}" for system in ["linux", "macos", "windows"]
                             for architecture in ["x86_64", "aarch64"]}
                valid_path = (len(parts) == 1 or
                              len(parts) == 3 and parts[0] == "native" and parts[1] in platforms)
                if (not valid_path or entry.filename != pathlib.PurePosixPath(entry.filename).as_posix()
                        or "\\" in entry.filename or "." in parts or ".." in parts
                        or not entry.filename.endswith(".hyaplugin")
                        or entry.filename in names or entry.file_size > 16 * 1024 * 1024):
                    raise ValueError("invalid official bundle entry")
                total += entry.file_size
                if len(names) >= 64 or total > 64 * 1024 * 1024:
                    raise ValueError("official bundle exceeds extraction limit")
                names.add(entry.filename)
                target = staging / entry.filename
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.read(entry))
            if not names:
                raise ValueError("empty official bundle")
        (staging / "bundle-version.txt").write_text(version + "\n")
        if destination.exists():
            shutil.rmtree(destination)
        staging.rename(destination)
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    print(stage(pathlib.Path(__file__).resolve().parents[2], args.version))


if __name__ == "__main__":
    main()
