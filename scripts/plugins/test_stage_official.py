"""Offline scenarios for official release bundle staging."""
import importlib.util
import io
import pathlib
import tempfile
import unittest
import warnings
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("stage_official", pathlib.Path(__file__).with_name("stage-official.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def bundle(entries):
    stream = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)
        with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, data in entries:
                entry = zipfile.ZipInfo()
                entry.filename = name
                entry.orig_filename = name
                entry.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(entry, data)
    return stream.getvalue()


class StageOfficialTests(unittest.TestCase):
    def test_windows_normalization_does_not_hide_backslashes(self):
        name = "native\\linux-x86_64\\torrent.hyaplugin"
        data = bundle([(name, b"bad")])
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            destination = root / "plugins/bundled"
            destination.mkdir(parents=True)
            (destination / "bundle-version.txt").write_text("1.0.0")
            (destination / "youtube.hyaplugin").write_bytes(b"retained")
            with patch.object(zipfile.os, "sep", "\\"):
                with zipfile.ZipFile(io.BytesIO(data)) as archive:
                    entry = archive.infolist()[0]
                    self.assertEqual(entry.orig_filename, name)
                    self.assertEqual(entry.filename, name.replace("\\", "/"))
                with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(data)):
                    with self.assertRaisesRegex(ValueError, "invalid official bundle entry"):
                        module.stage(root, "1.1.0")
            self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"retained")
            self.assertEqual((destination / "bundle-version.txt").read_text(), "1.0.0")

    def test_stages_portable_and_all_native_architectures(self):
        entries = [("youtube.hyaplugin", b"portable")]
        entries += [(f"native/{system}-{architecture}/torrent.hyaplugin", b"native")
                    for system in ["macos", "linux", "windows"]
                    for architecture in ["x86_64", "aarch64"]]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle(entries))):
                destination = module.stage(root, "1.0.0")
            for name, expected in entries:
                self.assertEqual((destination / name).read_bytes(), expected)

    def test_installs_release_and_refreshes_stale_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle([("youtube.hyaplugin", b"v1")]))) as fetch:
                destination = module.stage(root, "1.0.0")
                self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"v1")
                fetch.assert_called_once_with("https://github.com/ja7ad/hydra/releases/download/v1.0.0/hydra-official-plugins.zip", timeout=60)
            with patch.object(module.urllib.request, "urlopen") as fetch:
                self.assertEqual(module.stage(root, "1.0.0"), destination)
                fetch.assert_not_called()
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle([("youtube.hyaplugin", b"v2")]))):
                module.stage(root, "1.1.0")
            self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"v2")
            self.assertEqual((destination / "bundle-version.txt").read_text().strip(), "1.1.0")

    def test_bad_bundles_leave_previous_release_intact(self):
        for entries in [[], [("../escape.hyaplugin", b"bad")], [("key.pub", b"bad")],
                        [("a.hyaplugin", b"first"), ("a.hyaplugin", b"duplicate")],
                        [("native/linux-x86_64/../../escape.hyaplugin", b"bad")],
                        [("native/freebsd-x86_64/torrent.hyaplugin", b"bad")],
                        [("native//linux-x86_64/torrent.hyaplugin", b"bad")],
                        [("native\\linux-x86_64\\torrent.hyaplugin", b"bad")]]:
            with self.subTest(entries=entries), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                destination = root / "plugins/bundled"
                destination.mkdir(parents=True)
                (destination / "bundle-version.txt").write_text("1.0.0")
                (destination / "youtube.hyaplugin").write_bytes(b"retained")
                with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle(entries))):
                    with self.assertRaises(ValueError):
                        module.stage(root, "1.1.0")
                self.assertEqual((destination / "youtube.hyaplugin").read_bytes(), b"retained")

    def test_extraction_limit_rejects_too_many_packages(self):
        data = bundle([(f"plugin-{index}.hyaplugin", b"package") for index in range(65)])
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(data)):
                with self.assertRaisesRegex(ValueError, "extraction limit"):
                    module.stage(root, "1.0.0")
            self.assertFalse((root / "plugins/bundled").exists())

    def test_rejects_oversized_package_and_compressed_bundle(self):
        for entries in [[("large.hyaplugin", b"x" * (16 * 1024 * 1024 + 1))],
                        [(f"plugin-{index}.hyaplugin", b"x" * (16 * 1024 * 1024)) for index in range(5)]]:
            with tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(bundle(entries))):
                    with self.assertRaises(ValueError):
                        module.stage(root, "1.0.0")
                self.assertFalse((root / "plugins/bundled").exists())

    def test_rejects_oversized_download(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", return_value=io.BytesIO(b"x" * (64 * 1024 * 1024 + 1))):
                with self.assertRaisesRegex(ValueError, "exceeds 64 MiB"):
                    module.stage(root, "1.0.0")
            self.assertFalse((root / "plugins/bundled").exists())

    def test_network_failure_does_not_create_partial_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.object(module.urllib.request, "urlopen", side_effect=OSError("offline")):
                with self.assertRaises(OSError):
                    module.stage(root, "1.0.0")
            self.assertFalse((root / "plugins/bundled").exists())


if __name__ == "__main__":
    unittest.main()
