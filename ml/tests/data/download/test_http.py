"""The downloader against a local server that supports Range requests."""

import hashlib
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from rohditor_ml.data.download.files import Checksum, RemoteFile
from rohditor_ml.data.download.http import ChecksumError, download

PAYLOAD = bytes(range(256)) * 4096  # 1 MiB


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        start = 0
        if (range_header := self.headers.get("Range")):
            start = int(range_header.removeprefix("bytes=").rstrip("-"))
        body = PAYLOAD[start:]
        self.send_response(206 if start else 200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


@pytest.fixture
def url():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{server.server_port}/file.bin"
    server.shutdown()


def remote(url, digest=None):
    digest = digest or hashlib.sha256(PAYLOAD).hexdigest()
    return RemoteFile("test", "file.bin", url, len(PAYLOAD), "CC0-1.0", Checksum("sha256", digest))


def test_download_then_cached(url, tmp_path):
    assert download(remote(url), tmp_path) == "downloaded"
    assert (tmp_path / "file.bin").read_bytes() == PAYLOAD
    assert download(remote(url), tmp_path) == "cached"


def test_resumes_partial_file(url, tmp_path):
    (tmp_path / "file.bin.part").write_bytes(PAYLOAD[:300_000])
    assert download(remote(url), tmp_path) == "downloaded"
    assert (tmp_path / "file.bin").read_bytes() == PAYLOAD


def test_bad_checksum_leaves_no_final_file(url, tmp_path):
    with pytest.raises(ChecksumError):
        download(remote(url, digest="0" * 64), tmp_path)
    assert not (tmp_path / "file.bin").exists()
    assert not (tmp_path / "file.bin.part").exists()


def test_verify_replaces_corrupt_file(url, tmp_path):
    (tmp_path / "file.bin").write_bytes(b"x" * len(PAYLOAD))  # right size, wrong content
    assert download(remote(url), tmp_path) == "cached"  # size-only check trusts it
    assert download(remote(url), tmp_path, verify=True) == "downloaded"
    assert (tmp_path / "file.bin").read_bytes() == PAYLOAD
