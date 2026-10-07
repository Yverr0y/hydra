"""Resolve magnets through a local BEP 9 peer without a libtorrent Python wheel."""
import ctypes
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import socketserver
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse

from webseed import bencode


def receive(connection, size):
    result = bytearray()
    while len(result) < size:
        chunk = connection.recv(size - len(result))
        if not chunk:
            return None
        result.extend(chunk)
    return bytes(result)


def main():
    library, cli, package = [Path(value).resolve() for value in sys.argv[1:]]
    info = bencode({b"name": b"magnet-fixture.bin", b"length": 1,
                    b"piece length": 16384, b"pieces": hashlib.sha1(b"x").digest()})
    info_hash = hashlib.sha1(info).digest()
    failures = []
    requests = []

    class Peer(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.settimeout(10)
            try:
                handshake = receive(self.request, 68)
                if handshake is None:
                    return
                assert handshake[:20] == b"\x13BitTorrent protocol", handshake
                assert handshake[28:48] == info_hash, handshake
                self.request.sendall(b"\x13BitTorrent protocol" + b"\x00" * 5 + b"\x10\x00\x00"
                                     + info_hash + b"-HY0001-" + b"0" * 12)

                def send(payload):
                    self.request.sendall(struct.pack("!I", len(payload)) + payload)

                send(b"\x14\x00" + bencode({b"m": {b"ut_metadata": 1}, b"metadata_size": len(info)}))
                metadata_extension = None
                while True:
                    header = receive(self.request, 4)
                    if header is None:
                        return
                    size, = struct.unpack("!I", header)
                    assert size <= 16384, size
                    if size == 0:
                        continue
                    message = receive(self.request, size)
                    if message is None:
                        return
                    assert message[0] != 6, "metadata inspection requested payload pieces"
                    if message[:2] == b"\x14\x00":
                        extension = re.search(rb"11:ut_metadatai([0-9]+)e", message[2:])
                        assert extension is not None, message
                        metadata_extension = int(extension[1])
                    if message[:2] == b"\x14\x01":
                        assert metadata_extension is not None
                        requests.append(message)
                        send(bytes([20, metadata_extension]) + bencode({b"msg_type": 1, b"piece": 0,
                                                      b"total_size": len(info)}) + info)
            except (ConnectionResetError, BrokenPipeError):
                return
            except Exception as error:
                failures.append(error)

    class Server(socketserver.ThreadingTCPServer):
        block_on_close = True

    emit_type = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t)
    poll_type = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint64))
    native = ctypes.CDLL(str(library))
    native.hydra_native_v1.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p,
                                     ctypes.c_size_t, emit_type, poll_type, ctypes.c_void_p]
    native.hydra_native_v1.restype = ctypes.c_int32

    def inspect(magnet, cancelled=False, reject=False):
        frames = []
        deadline = time.monotonic() + 10

        @emit_type
        def emit(_, address, size):
            frames.append(json.loads(ctypes.string_at(address, size)))
            return int(reject)

        @poll_type
        def poll(_, limit):
            limit[0] = (1 << 64) - 1
            return int(cancelled or time.monotonic() >= deadline)

        payload = json.dumps({"magnet": magnet}).encode()
        status = native.hydra_native_v1(b"inspect", 7, payload, len(payload), emit, poll, None)
        return status, frames

    def assert_plan(plan):
        assert plan["id"] == info_hash.hex(), plan
        assert plan["transfer"]["files"] == [{"index": 0, "path": "magnet-fixture.bin", "size": 1}], plan
        assert plan["transfer"]["output"] == "file", plan

    def inspect_with_https_tracker(peer_port, version):
        handshakes = []
        peers = b"\x7f\x00\x00\x01" + struct.pack("!H", peer_port)

        class Tracker(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                response = bencode({b"interval": 60, b"peers": peers})
                self.send_response(200)
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

            def log_message(self, *_):
                pass

        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = context.maximum_version = version
        fixtures = Path(__file__).parent / "fixtures"
        context.load_cert_chain(fixtures / "tracker-cert.pem", fixtures / "tracker-key.pem")

        class HttpsTracker(http.server.ThreadingHTTPServer):
            def get_request(self):
                connection, address = super().get_request()
                connection.settimeout(10)
                try:
                    connection = context.wrap_socket(connection, server_side=True)
                except ssl.SSLError:
                    handshakes.append("certificate rejected")
                    connection.close()
                    raise
                handshakes.append(connection.version())
                return connection, address

        with HttpsTracker(("127.0.0.1", 0), Tracker) as https_tracker, http.server.ThreadingHTTPServer(("127.0.0.1", 0), Tracker) as http_tracker:
            threads = [threading.Thread(target=server.serve_forever) for server in (https_tracker, http_tracker)]
            for thread in threads:
                thread.start()
            try:
                trackers = [f"https://127.0.0.1:{https_tracker.server_port}/announce",
                            f"http://127.0.0.1:{http_tracker.server_port}/announce"]
                magnet = f"magnet:?xt=urn:btih:{info_hash.hex()}" + "".join(
                    "&tr=" + urllib.parse.quote(tracker, safe="") for tracker in trackers)
                status, frames = inspect(magnet)
                assert status == 0, frames
                assert_plan(frames[-1])
                assert handshakes, "magnet resolution skipped the HTTPS tracker"
            finally:
                https_tracker.shutdown()
                http_tracker.shutdown()
                for thread in threads:
                    thread.join()

    with Server(("127.0.0.1", 0), Peer) as server:
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        magnet = f"magnet:?xt=urn:btih:{info_hash.hex()}&x.pe=127.0.0.1:{server.server_address[1]}"
        try:
            status, frames = inspect("magnet:?xt=invalid")
            assert status != 0 and frames[-1].get("error"), frames
            status, frames = inspect(magnet, cancelled=True)
            assert status != 0 and "cancelled" in frames[-1]["error"], frames
            for _ in range(2):
                status, frames = inspect(magnet)
                assert status == 0, frames
                assert_plan(frames[-1])
            status, frames = inspect(magnet, reject=True)
            assert status != 0 and "rejected" in frames[-1]["error"], frames
            for version in (ssl.TLSVersion.TLSv1_2, ssl.TLSVersion.TLSv1_3):
                inspect_with_https_tracker(server.server_address[1], version)
            with tempfile.TemporaryDirectory() as temporary:
                environment = dict(os.environ, HYDRA_CONFIG_DIR=str(Path(temporary) / "profile"))
                subprocess.run([str(cli), "plugin", "install", str(package), "--accept-permissions"],
                               env=environment, check=True, timeout=30, capture_output=True)
                for _ in range(2):
                    result = subprocess.run([str(cli), "plugin", "resolve", magnet], env=environment,
                                            check=True, timeout=30, capture_output=True, text=True)
                    plugin, plan = json.loads(result.stdout)
                    assert plugin == "hydra.torrent", plugin
                    assert_plan(plan)
        finally:
            server.shutdown()
            thread.join()
    assert not failures, failures
    assert len(requests) >= 5, requests
    print("PASS: magnet metadata, TLS 1.2/1.3 tracker fallback, cancellation, invalid input, rejected callback and installed CLI resolution")


if __name__ == "__main__":
    main()
