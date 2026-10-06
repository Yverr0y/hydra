"""Real native and CLI transfers without a platform-specific Python libtorrent wheel."""
import ctypes
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time


def bencode(value):
    if isinstance(value, int):
        return b"i" + str(value).encode() + b"e"
    if isinstance(value, bytes):
        return str(len(value)).encode() + b":" + value
    if isinstance(value, dict):
        return b"d" + b"".join(bencode(key) + bencode(value[key]) for key in sorted(value)) + b"e"
    raise TypeError(type(value))


def main():
    library, cli, package = [Path(value).resolve() for value in sys.argv[1:]]
    content = hashlib.sha256(b"Hydra webseed fixture").digest() * 2048

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            start, end = 0, len(content) - 1
            interval = self.headers.get("Range")
            if interval:
                first, last = interval.removeprefix("bytes=").split("-", 1)
                start = int(first)
                end = min(int(last) if last else end, end)
            self.send_response(206 if interval else 200)
            self.send_header("Content-Length", str(end - start + 1))
            self.send_header("Accept-Ranges", "bytes")
            if interval:
                self.send_header("Content-Range", f"bytes {start}-{end}/{len(content)}")
            self.end_headers()
            self.wfile.write(content[start:end + 1])

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    pieces = b"".join(hashlib.sha1(content[offset:offset + 16384]).digest()
                      for offset in range(0, len(content), 16384))
    metainfo = bencode({b"info": {b"name": b"fixture.bin", b"length": len(content),
                                b"piece length": 16384, b"pieces": pieces, b"private": 1},
                       b"url-list": f"http://127.0.0.1:{server.server_port}/fixture.bin".encode()})
    emit_type = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t)
    poll_type = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint64))
    native = ctypes.CDLL(str(library))
    native.hydra_native_v1.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p,
                                     ctypes.c_size_t, emit_type, poll_type, ctypes.c_void_p]
    native.hydra_native_v1.restype = ctypes.c_int32

    def call(method, request):
        frames = []
        deadline = time.monotonic() + 25

        @emit_type
        def emit(_, address, length):
            frames.append(json.loads(ctypes.string_at(address, length)))
            return 0

        @poll_type
        def poll(_, limit):
            limit[0] = (1 << 64) - 1
            return int(time.monotonic() >= deadline)

        payload = json.dumps(request).encode()
        method = method.encode()
        result = native.hydra_native_v1(method, len(method), payload, len(payload), emit, poll, None)
        return result, frames

    try:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            status, frames = call("inspect", {"metainfo_hex": "00"})
            assert status != 0 and frames[-1].get("error"), frames
            status, frames = call("inspect", {"metainfo_hex": metainfo.hex()})
            assert status == 0, frames
            transfer = frames[-1]["transfer"]
            status, frames = call("download", {"transfer": transfer, "files": None,
                                               "destination": str(root / "native.bin"),
                                               "resume_path": str(root / "native.resume")})
            assert status == 0 and frames[-1]["state"] == "complete", frames[-3:]
            assert (root / "native.bin").read_bytes() == content
            assert (root / "native.resume").is_file()
            environment = dict(os.environ, HYDRA_CONFIG_DIR=str(root / "profile"))
            subprocess.run([str(cli), "plugin", "install", str(package), "--accept-permissions"],
                           env=environment, check=True, timeout=30)
            source = root / "sample file.torrent"
            source.write_bytes(metainfo)
            subprocess.run([str(cli), str(source), "--output-dir", str(root / "cli"),
                            "--no-proxy", "--quiet"], env=environment, check=True, timeout=30)
            assert (root / "cli/fixture.bin").read_bytes() == content
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    print("PASS: native ABI, invalid metadata, real webseed transfer, checkpoint and installed CLI")


if __name__ == "__main__":
    main()
