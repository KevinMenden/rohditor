"""Resumable, checksum-verified HTTP downloads using only the standard library."""

from __future__ import annotations

import hashlib
import http.client
import os
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from .files import Checksum, RemoteFile

USER_AGENT = "rohditor-ml/0.1 (+https://github.com/KevinMenden/rohditor)"
CHUNK = 1 << 20
TIMEOUT = 60
# Transient failures worth retrying; each retry resumes from the `.part` file.
_RETRYABLE = (urllib.error.URLError, TimeoutError, ConnectionError, http.client.HTTPException)


class ChecksumError(Exception):
    pass


def _open(url: str, headers: dict[str, str] | None = None):
    # Some upstream URLs contain spaces; quote them without double-encoding "%XX".
    url = urllib.parse.quote(url, safe=":/?=&%+")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, **(headers or {})})
    return urllib.request.urlopen(request, timeout=TIMEOUT)


def _is_retryable(error: Exception) -> bool:
    if isinstance(error, urllib.error.HTTPError):
        return error.code == 429 or error.code >= 500
    return isinstance(error, _RETRYABLE)


def _with_retries(action, retries: int):
    for attempt in range(retries + 1):
        try:
            return action()
        except Exception as error:  # noqa: BLE001 - re-raised unless retryable
            if attempt == retries or not _is_retryable(error):
                raise
            time.sleep(min(2**attempt, 30))


def get_bytes(url: str, retries: int = 4) -> bytes:
    def fetch() -> bytes:
        with _open(url) as response:
            return response.read()

    return _with_retries(fetch, retries)


def get_text(url: str, retries: int = 4) -> str:
    return get_bytes(url, retries).decode("utf-8", errors="replace")


def cached_text(url: str, path: Path) -> str:
    """Fetch small metadata once and keep it on disk. Delete the file to refresh."""
    if path.exists():
        return path.read_text(encoding="utf-8")
    text = get_text(url)
    path.parent.mkdir(parents=True, exist_ok=True)
    part = path.with_name(path.name + ".part")
    part.write_text(text, encoding="utf-8")
    os.replace(part, path)
    return text


def file_checksum(path: Path, algorithm: str) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, algorithm).hexdigest()


def _matches(path: Path, checksum: Checksum | None) -> bool:
    return checksum is None or file_checksum(path, checksum.algorithm) == checksum.hexdigest


def _stream_to(part: Path, file: RemoteFile) -> None:
    """Append the remote bytes to `part`, resuming where it left off."""
    offset = part.stat().st_size if part.exists() else 0
    headers = {"Range": f"bytes={offset}-"} if offset else {}
    try:
        response = _open(file.url, headers)
    except urllib.error.HTTPError as error:
        if error.code == 416:  # the part already holds the whole file
            return
        raise
    with response:
        # A server that ignores Range answers 200 with the full body: start over.
        resumed = offset > 0 and response.status == 206
        with part.open("ab" if resumed else "wb") as out:
            while chunk := response.read(CHUNK):
                out.write(chunk)


def download(file: RemoteFile, directory: Path, *, verify: bool = False, retries: int = 5) -> str:
    """Download `file` into `directory`; returns "cached" or "downloaded".

    Data goes to `<name>.part` and is renamed only after the checksum matches, so
    a file under its final name is always complete. Re-running resumes partial
    transfers. `verify` re-hashes files that are already present.
    """
    target = directory / file.name
    if target.exists():
        if not verify and (not file.exact_size or target.stat().st_size == file.size):
            return "cached"
        if verify and _matches(target, file.checksum):
            return "cached"
        target.unlink()  # wrong size or checksum: fetch it again

    target.parent.mkdir(parents=True, exist_ok=True)
    part = target.with_name(target.name + ".part")
    _with_retries(lambda: _stream_to(part, file), retries)

    if file.exact_size and part.stat().st_size != file.size:
        part.unlink()
        raise ChecksumError(f"{file.name}: expected {file.size} bytes, got a different size")
    if not _matches(part, file.checksum):
        part.unlink()
        raise ChecksumError(f"{file.name}: {file.checksum.algorithm} mismatch")
    os.replace(part, target)
    return "downloaded"
