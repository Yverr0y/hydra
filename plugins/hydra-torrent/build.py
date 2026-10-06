"""Build a platform-specific native torrent package with the Hydra authoring CLI."""
import argparse
import os
from pathlib import Path
import platform
import re
import tomllib
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent
REPOSITORY = ROOT.parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--native-build", type=Path, default=REPOSITORY / "target/torrent-native-static")
    parser.add_argument("--system-engine", action="store_true", help="Link an installed libtorrent; the resulting package requires its shared libraries")
    args = parser.parse_args()
    system = {"Darwin": "macos", "Linux": "linux", "Windows": "windows"}[platform.system()]
    architecture = {"arm64": "aarch64", "ARM64": "aarch64", "aarch64": "aarch64", "AMD64": "x86_64", "x86_64": "x86_64"}[platform.machine()]
    key = f"{system}-{architecture}"
    suffix = {"macos": "dylib", "linux": "so", "windows": "dll"}[system]
    output = (args.output or REPOSITORY / "target" / f"torrent-download-{key}.hyaplugin").resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    build = args.native_build.resolve()
    configure = ["cmake", "-S", str(ROOT / "native"), "-B", str(build), "-DCMAKE_BUILD_TYPE=Release",
                 f"-DHYDRA_STATIC_ENGINE={'OFF' if args.system_engine else 'ON'}"]
    for key_name in ["CMAKE_TOOLCHAIN_FILE", "VCPKG_TARGET_TRIPLET", "CMAKE_PREFIX_PATH"]:
        if os.environ.get(key_name): configure.append(f"-D{key_name}={os.environ[key_name]}")
    subprocess.run(configure, check=True)
    subprocess.run(["cmake", "--build", str(build), "--config", "Release", "--parallel", "4"], check=True)
    library = build / f"hydra-torrent-native.{suffix}"
    if not library.is_file():
        library = build / "Release" / library.name
    with tempfile.TemporaryDirectory(prefix="hydra-torrent-build-") as temporary:
        staging = Path(temporary)
        shutil.copytree(ROOT / "src", staging / "src")
        cargo = (ROOT / "Cargo.toml").read_text().replace("../../crates/hydra-plugin-sdk", (REPOSITORY / "crates/hydra-plugin-sdk").as_posix())
        (staging / "Cargo.toml").write_text(cargo)
        shutil.copy(ROOT / "Cargo.lock", staging)
        shutil.copy(ROOT / "hydra-project.toml", staging)
        manifest = (ROOT / "hydra-plugin.toml").read_text()
        modules = tomllib.loads(manifest)["native_modules"]
        selected = next(module for module in modules if module["platform"] == key)
        manifest = re.sub(r"\[\[native_modules\]\]\n.*?(?=\n\[|\Z)", "", manifest, flags=re.S)
        native = f'[[native_modules]]\nid = "{selected["id"]}"\nplatform = "{key}"\nmodule = "{selected["module"]}"\n\n'
        manifest = manifest.replace("[permissions]", native + "[permissions]", 1)
        (staging / "hydra-plugin.toml").write_text(manifest)
        shutil.copy(library, staging / selected["module"])
        license_text = "Hydra torrent plugin: MIT OR Apache-2.0.\n\n"
        for source in [REPOSITORY / "LICENSE-MIT", REPOSITORY / "LICENSE-APACHE", build / "_deps/libtorrent-src/LICENSE", ROOT / "native/third-party-licenses.txt"]:
            if source.is_file():
                license_text += source.read_text() + "\n\n"
        (staging / "LICENSE").write_text(license_text)
        tool = ["cargo", "run", "--manifest-path", str(REPOSITORY / "Cargo.toml"), "-p", "hya-plugin-cli", "--"]
        subprocess.run(tool + ["build", str(staging), "--output", str(output)], check=True)
        subprocess.run(tool + ["validate", str(output)], check=True)
    print(output)


if __name__ == "__main__":
    main()
