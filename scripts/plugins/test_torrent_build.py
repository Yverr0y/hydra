"""Check cross-platform packaging and native configure arguments."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("torrent_build", ROOT / "plugins/hydra-torrent/build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class TorrentBuildTests(unittest.TestCase):
    def test_an_unknown_windows_architecture_is_rejected_before_building(self):
        result = subprocess.run(["bash", str(ROOT / "scripts/plugins/build-torrent-windows.sh"),
                                 "unsupported"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("Expected aarch64 or x86_64", result.stderr)

    def test_a_foreign_platform_requires_an_existing_native_build(self):
        with patch.object(sys, "argv", ["build.py", "--platform", "windows-aarch64"]), \
                patch.object(build.platform, "system", return_value="Darwin"), \
                patch.object(build.platform, "machine", return_value="arm64"), \
                patch.object(build.subprocess, "run") as run, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                build.main()
            self.assertEqual(error.exception.code, 2)
            run.assert_not_called()

    def test_a_cross_compiled_dll_is_packaged_for_windows_from_macos(self):
        for architecture in ("aarch64", "x86_64"):
            with self.subTest(architecture=architecture), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                (directory / "hydra-torrent-native.dll").write_bytes(b"cross-compiled DLL")
                (directory / "LICENSE-extra").write_text("Extra native dependency license")
                arguments = ["build.py", "--platform", f"windows-{architecture}", "--skip-native-build",
                             "--native-build", str(directory), "--output", str(directory / "torrent.hyaplugin")]
                staged = []

                def capture_package(command, **_):
                    if "build" in command:
                        staging = Path(command[command.index("build") + 1])
                        staged.append((staging / "hydra-plugin.toml").read_text())
                        self.assertEqual((staging / f"torrent-windows-{architecture}.dll").read_bytes(), b"cross-compiled DLL")
                        self.assertIn("Extra native dependency license", (staging / "LICENSE").read_text())

                with patch.object(sys, "argv", arguments), \
                        patch.object(build.platform, "system", return_value="Darwin"), \
                        patch.object(build.platform, "machine", return_value="arm64"), \
                        patch.object(build.subprocess, "run", side_effect=capture_package), contextlib.redirect_stdout(io.StringIO()):
                    build.main()
                self.assertEqual(len(staged), 1)
                self.assertIn(f'platform = "windows-{architecture}"', staged[0])
                self.assertNotIn('platform = "macos-aarch64"', staged[0])
                self.assertIn("Extra native dependency license", (directory / "torrent.LICENSE").read_text())

    def test_windows_configure_uses_the_installed_dependencies(self):
        with tempfile.TemporaryDirectory(prefix="torrent build ") as temporary:
            directory = Path(temporary)
            native = directory / "native"
            (native / "Release").mkdir(parents=True)
            (native / "Release/hydra-torrent-native.dll").write_bytes(b"test DLL")
            environment = {
                "CMAKE_TOOLCHAIN_FILE": str(directory / "vcpkg.cmake"),
                "VCPKG_TARGET_TRIPLET": "arm64-windows-static",
            }
            arguments = ["build.py", "--native-build", str(native),
                         "--output", str(directory / "torrent.hyaplugin")]
            with patch.dict(os.environ, environment, clear=True), patch.object(sys, "argv", arguments), \
                    patch.object(build.platform, "system", return_value="Windows"), \
                    patch.object(build.platform, "machine", return_value="ARM64"), \
                    patch.object(build.subprocess, "run") as run, contextlib.redirect_stdout(io.StringIO()):
                build.main()
            configure = run.call_args_list[0].args[0]
            self.assertEqual(configure[-2:], ["-A", "ARM64"])
            for key, value in environment.items():
                self.assertIn(f"-D{key}={value}", configure)


if __name__ == "__main__":
    unittest.main()
