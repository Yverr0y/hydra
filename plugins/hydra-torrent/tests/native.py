"""Local-swarm integration tests for the imported libtorrent engine."""
import ctypes
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import struct
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse

LIBRARY = Path(sys.argv.pop(1)).resolve()
EMIT = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t)
POLL = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint64))
NATIVE = ctypes.CDLL(str(LIBRARY))
NATIVE.hydra_native_v1.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p, ctypes.c_size_t, EMIT, POLL, ctypes.c_void_p]
NATIVE.hydra_native_v1.restype = ctypes.c_int32

# Load Hydra first so the seeder wheel does not select its C++ runtime.
import libtorrent as lt


def call(method, request, stop=None, progress=None):
    frames = []
    errors = []

    @EMIT
    def emit(_, address, size):
        try:
            frame = json.loads(ctypes.string_at(address, size))
            frames.append(frame)
            if progress:
                progress(frame)
            return 0
        except Exception as error:
            errors.append(error)
            return 1

    @POLL
    def poll(_, limit):
        limit[0] = (1 << 64) - 1
        return int(stop is not None and stop.is_set())

    if stop is None:
        stop = threading.Event()
    timer = threading.Timer(15, stop.set)
    timer.start()
    payload = json.dumps(request).encode()
    method = method.encode()
    try:
        status = NATIVE.hydra_native_v1(method, len(method), payload, len(payload), emit, poll, None)
    finally:
        timer.cancel()
    if errors:
        raise errors[0]
    return status, frames


def load(metadata):
    if hasattr(lt, "load_torrent_buffer"):
        return lt.load_torrent_buffer(metadata)
    params = lt.add_torrent_params()
    params.ti = lt.torrent_info(lt.bdecode(metadata))
    return params


class Swarm:
    def __init__(self, root, flags, private=True, failing_tracker=False):
        self.seed = root / "seed"
        (self.seed / "bundle").mkdir(parents=True)
        self.contents = {"bundle/a.bin": hashlib.sha256(b"A").digest() * 1701,
                         "bundle/b.bin": hashlib.sha256(b"B").digest() * 2303}
        storage = lt.file_storage()
        for name, content in self.contents.items():
            (self.seed / name).write_bytes(content)
            storage.add_file(name, len(content))
        self.session = lt.session({"listen_interfaces": "127.0.0.1:0", "peer_fingerprint": "-HS0001-", "enable_dht": False,
                                   "enable_lsd": False, "enable_upnp": False, "enable_natpmp": False})
        deadline = time.monotonic() + 5
        while self.session.listen_port() == 0 and time.monotonic() < deadline:
            self.session.wait_for_alert(100)
            self.session.pop_alerts()
        self.port = self.session.listen_port()
        if not self.port: raise RuntimeError("local seeder did not open its listening port")
        seed_port = self.port
        peer = socket.inet_aton("127.0.0.1") + struct.pack("!H", seed_port)

        class Tracker(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            def do_GET(self):
                parameters = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
                peers = b"" if parameters.get("port") == [str(seed_port)] else peer
                response = lt.bencode({b"interval": 1, b"peers": peers, b"complete": 1, b"incomplete": 1})
                if urllib.parse.urlsplit(self.path).path == "/unavailable":
                    response = lt.bencode({b"failure reason": b"tracker temporarily unavailable"})
                self.send_response(200)
                self.send_header("Content-Length", str(len(response)))
                self.send_header("Connection", "close")
                self.end_headers()
                try:
                    self.wfile.write(response)
                    self.wfile.flush()
                except BrokenPipeError:
                    pass

            def log_message(self, *_):
                pass

        self.tracker = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Tracker)
        self.thread = threading.Thread(target=self.tracker.serve_forever, daemon=True)
        self.thread.start()
        creator = lt.create_torrent(storage, 16384, flags)
        creator.set_priv(private)
        if failing_tracker:
            creator.add_tracker(f"http://127.0.0.1:{self.tracker.server_port}/unavailable", 0)
        creator.add_tracker(f"http://127.0.0.1:{self.tracker.server_port}/announce", 1 if failing_tracker else 0)
        lt.set_piece_hashes(creator, str(self.seed))
        self.metainfo = lt.bencode(creator.generate())
        params = load(self.metainfo)
        params.save_path = str(self.seed)
        params.flags &= ~(lt.torrent_flags.paused | lt.torrent_flags.auto_managed)
        self.handle = self.session.add_torrent(params)
        deadline = time.monotonic() + 5
        while not self.handle.status().is_seeding and time.monotonic() < deadline:
            self.session.wait_for_alert(100)
            self.session.pop_alerts()
        if not self.handle.status().is_seeding: raise RuntimeError("local seeder did not verify its fixture files")

    def close(self):
        self.tracker.shutdown()
        self.tracker.server_close()
        self.session.remove_torrent(self.handle)


class NativeTorrentTests(unittest.TestCase):
    def test_failed_tracker_is_debug_and_other_tracker_completes_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            swarm = Swarm(root, lt.create_torrent.v1_only, failing_tracker=True)
            try:
                status, frames = call("inspect", {"metainfo_hex": swarm.metainfo.hex()})
                self.assertEqual(status, 0, frames)
                transfer = frames[-1]["transfer"]
                transfer["metadata"]["listen_interfaces"] = "127.0.0.1:0"
                status, frames = call("download", {"transfer": transfer, "destination": str(root / "output"),
                                                  "files": None, "download_limit": 16384,
                                                  "resume_path": str(root / "resume")})
                self.assertEqual(status, 0, frames)
                self.assertEqual(frames[-1]["state"], "complete", frames[-3:])
                logs = [record for frame in frames for record in frame.get("logs", [])]
                tracker_logs = [record for record in logs if record["message"].startswith("Tracker error:")]
                self.assertTrue(tracker_logs, logs)
                self.assertTrue(all(record["level"] == "debug" for record in tracker_logs))
                self.assertEqual(len(tracker_logs), len({record["message"] for record in tracker_logs}))
                for name, content in swarm.contents.items():
                    self.assertEqual((root / "output" / name.removeprefix("bundle/")).read_bytes(), content)
            finally:
                swarm.close()

    def test_v1_v2_and_hybrid_select_files_verify_bytes_and_recover_corruption(self):
        for flags in [lt.create_torrent.v1_only, lt.create_torrent.v2_only, 0]:
            with self.subTest(flags=flags), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                swarm = Swarm(root, flags)
                try:
                    status, frames = call("inspect", {"metainfo_hex": swarm.metainfo.hex()})
                    self.assertEqual(status, 0, frames)
                    transfer = frames[-1]["transfer"]
                    transfer["metadata"]["listen_interfaces"] = "127.0.0.1:0"
                    selected = next(f["index"] for f in transfer["files"] if f["path"] == "b.bin")
                    destination = root / "output"
                    request = {"transfer": transfer, "destination": str(destination), "files": [selected],
                               "download_limit": 16384, "resume_path": str(root / "resume")}
                    deadline = threading.Timer(25, lambda: stop.set())
                    stop = threading.Event()
                    deadline.start()
                    try:
                        status, frames = call("download", request, stop)
                    finally:
                        deadline.cancel()
                    self.assertEqual(status, 0, frames)
                    self.assertEqual(frames[-1]["state"], "complete", frames[-3:])
                    output = destination / "b.bin"
                    self.assertEqual(output.read_bytes(), swarm.contents["bundle/b.bin"])
                    self.assertTrue((root / "resume").is_file())
                    self.assertEqual(frames[-1]["done"], len(swarm.contents["bundle/b.bin"]))
                    peer_rows = [row for frame in frames for row in frame.get("details", [])]
                    self.assertTrue(any(row[0].startswith("127.0.0.1:") for row in peer_rows))
                    self.assertTrue(all(len(row) == len(transfer["details"]["columns"]) for row in peer_rows))
                    logs = [record for frame in frames for record in frame.get("logs", [])]
                    self.assertTrue(any(record["level"] == "info" and "started" in record["message"] for record in logs))
                    self.assertTrue(any(record["level"] == "debug" and "verification" in record["message"] for record in logs))
                    self.assertTrue(any("complete" in record["message"] for record in logs))
                    self.assertTrue(all("http" not in record["message"] and "magnet:" not in record["message"] for record in logs))
                    output.write_bytes(b"corrupt")
                    status, frames = call("download", request, threading.Event())
                    self.assertEqual(status, 0, frames)
                    self.assertEqual(output.read_bytes(), swarm.contents["bundle/b.bin"])
                finally:
                    swarm.close()

    def test_magnets_resolve_metadata_and_download_v1_v2_and_hybrid_files(self):
        for flags in [lt.create_torrent.v1_only, lt.create_torrent.v2_only, 0]:
            with self.subTest(flags=flags), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                swarm = Swarm(root, flags, private=False)
                try:
                    info = lt.torrent_info(lt.bdecode(swarm.metainfo))
                    magnet = lt.make_magnet_uri(info) + f"&x.pe=127.0.0.1:{swarm.port}"
                    status, frames = call("inspect", {"magnet":magnet})
                    self.assertEqual(status, 0, frames)
                    transfer = frames[-1]["transfer"]
                    transfer["metadata"]["listen_interfaces"] = "127.0.0.1:0"
                    request = {"transfer":transfer, "destination":str(root / "output"), "files":None,
                               "download_limit":0, "resume_path":str(root / "resume")}
                    status, frames = call("download", request)
                    self.assertEqual(status, 0, frames)
                    self.assertEqual(frames[-1]["state"], "complete", frames[-3:])
                    for name, content in swarm.contents.items():
                        self.assertEqual((root / "output" / name.removeprefix("bundle/")).read_bytes(), content)
                finally:
                    swarm.close()

    def test_pause_saves_resume_and_invalid_selection_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            swarm = Swarm(root, lt.create_torrent.v1_only)
            try:
                _, frames = call("inspect", {"metainfo_hex": swarm.metainfo.hex()})
                request = {"transfer": frames[-1]["transfer"], "destination": str(root / "output"),
                           "files": None, "download_limit": 1, "resume_path": str(root / "resume")}
                stop = threading.Event()
                stop.set()
                status, frames = call("download", request, stop)
                self.assertEqual(status, 0, frames)
                self.assertEqual(frames[-1]["state"], "stopped")
                self.assertTrue((root / "resume").is_file())
                request["files"] = [9999]
                status, frames = call("download", request)
                self.assertNotEqual(status, 0)
                self.assertIn("selection", frames[-1]["error"])
            finally:
                swarm.close()

    def test_invalid_metadata_and_symlink_destinations_are_refused(self):
        status, frames = call("inspect", {"metainfo_hex": "6465"})
        self.assertNotEqual(status, 0)
        self.assertIn("error", frames[-1])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            swarm = Swarm(root, lt.create_torrent.v1_only)
            try:
                _, frames = call("inspect", {"metainfo_hex": swarm.metainfo.hex()})
                (root / "outside").mkdir()
                try:
                    (root / "output").symlink_to(root / "outside", target_is_directory=True)
                except OSError:
                    self.skipTest("symlink creation requires a platform privilege")
                request = {"transfer": frames[-1]["transfer"], "destination": str(root / "output"),
                           "files": None, "download_limit": 0, "resume_path": str(root / "resume")}
                status, frames = call("download", request)
                self.assertNotEqual(status, 0)
                self.assertIn("symlink", frames[-1]["error"])
                self.assertEqual(list((root / "outside").iterdir()), [])
            finally:
                swarm.close()


if __name__ == "__main__":
    unittest.main()
