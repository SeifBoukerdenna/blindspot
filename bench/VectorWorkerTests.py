"""Fixture-only integration tests for the isolated native vector worker."""
import hashlib
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import time


class Worker:
    def __init__(self, executable, directory):
        self.process = subprocess.Popen([executable], cwd=directory, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.sequence = 0
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)

    def raw(self, request):
        self.process.stdin.write(json.dumps(request).encode() + b"\n")
        self.process.stdin.flush()
        response = bytearray()
        deadline = time.monotonic() + 10
        while b"\n" not in response:
            assert time.monotonic() < deadline, "worker reply timed out"
            if not self.selector.select(timeout=0.1):
                continue
            data = os.read(self.process.stdout.fileno(), 4096)
            assert data, "worker disconnected"
            response.extend(data)
            assert len(response) <= 32768, "response exceeds protocol limit"
        assert response.endswith(b"\n") and response.count(b"\n") == 1
        return json.loads(response)

    def request(self, operation, **arguments):
        self.sequence += 1
        response = self.raw(dict(version=1, id=self.sequence,
                                 command=dict(operation=operation, **arguments)))
        assert response["version"] == 1
        assert response["id"] in (0, self.sequence)
        return response

    def close(self):
        self.selector.close()
        self.process.kill()
        self.process.communicate(timeout=2)


def build(worker, token, entries):
    assert "result" in worker.request("begin", token=token, dimensions=2, count=len(entries))
    assert worker.request("add", entries=entries)["result"]["count"] == len(entries)
    result = worker.request("finish")
    assert "result" in result, result
    return result["result"]


def query(worker, shards):
    return worker.request("query", dimensions=2, vector=[1.0, 0.0], shards=shards, limit=3)["result"]


def main():
    executable = str(Path(sys.argv[1]).resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix="blindspot-vector-test-") as temporary:
        root = Path(temporary)
        directory = root / "cache"
        directory.mkdir(mode=0o700)
        worker = Worker(executable, directory)
        try:
            first_token = "a" * 32
            first = build(worker, first_token, [dict(key=1, values=[1.0, 0.0]),
                                                dict(key=2, values=[0.0, 1.0]),
                                                dict(key=3, values=[-1.0, 0.0])])
            first_path = directory / (first_token + ".ann")
            assert first["checksum"] == hashlib.sha256(first_path.read_bytes()).hexdigest()
            assert first["bytes"] == first_path.stat().st_size
            assert first_path.stat().st_mode & 0o777 == 0o600
            shard = dict(token=first_token, checksum=first["checksum"])
            for _ in range(2):
                found = query(worker, [shard])
                assert found["unavailable"] == 0, found
                assert found["hits"][0]["key"] == 1, found
                assert abs(found["hits"][0]["distance"]) < 0.001
            second_token = "b" * 32
            second = build(worker, second_token, [dict(key=5, values=[1.0, 0.0]),
                                                  dict(key=6, values=[0.0, 1.0])])
            second_shard = dict(token=second_token, checksum=second["checksum"])
            assert "error" in worker.request("begin", token="../outside", dimensions=2, count=1)
            assert "error" in worker.request("begin", token="d" * 32, dimensions=2049, count=1)
            assert "error" in worker.request("begin", token="d" * 32, dimensions=2, count=65537)
            for entries in ([dict(key=0, values=[1.0, 0.0])],
                            [dict(key=1, values=[0.0, 0.0])],
                            [dict(key=1, values=[1.0])],
                            [dict(key=2, values=[1.0, 0.0]), dict(key=2, values=[1.0, 0.0])]):
                worker.request("begin", token="d" * 32, dimensions=2, count=2)
                assert "error" in worker.request("add", entries=entries)
            assert "error" in worker.request("executeShell", command="private-fixture-marker")
            assert "error" in worker.raw(dict(version=1, id=999, unexpected="private-fixture-marker",
                                               command=dict(operation="reset")))
            assert "error" in worker.request("reset", unexpected=True)
            assert "error" in worker.request("finish", unexpected=True)
            assert "error" in worker.request("query", dimensions=2, vector=[1.0, 0.0],
                                              shards=[shard], limit=101)
            worker.request("begin", token="d" * 32, dimensions=2, count=1)
            assert "error" in worker.request("finish")
            worker.request("begin", token=first_token, dimensions=2, count=1)
            worker.request("add", entries=[dict(key=9, values=[1.0, 0.0])])
            assert "error" in worker.request("finish"), "existing shards must not be overwritten"
            assert first["checksum"] == hashlib.sha256(first_path.read_bytes()).hexdigest()

            original = first_path.stat()
            with first_path.open("r+b") as file:
                file.seek(-1, os.SEEK_END)
                byte = file.read(1)
                file.seek(-1, os.SEEK_END)
                file.write(bytes([byte[0] ^ 1]))
            os.utime(first_path, ns=(original.st_atime_ns, original.st_mtime_ns))
            found = query(worker, [shard, second_shard])
            assert found["unavailable"] == 1 and found["hits"][0]["key"] == 5, found

            sentinel = root / "sentinel"
            sentinel.write_text("private-fixture-marker")
            (directory / ("c" * 32 + ".ann")).symlink_to(sentinel)
            found = query(worker, [dict(token="c" * 32, checksum=hashlib.sha256(sentinel.read_bytes()).hexdigest())])
            assert found["unavailable"] == 1 and not found["hits"]
            assert sentinel.read_text() == "private-fixture-marker"
            hardlink = root / "hardlink"
            os.link(directory / (second_token + ".ann"), hardlink)
            assert query(worker, [second_shard])["unavailable"] == 1
            hardlink.unlink()
            assert query(worker, [second_shard])["hits"][0]["key"] == 5
        finally:
            worker.close()

        reopened = Worker(executable, directory)
        try:
            assert query(reopened, [second_shard])["hits"][0]["key"] == 5
        finally:
            reopened.close()
        for payload in [b"x" * (2 * 1024 * 1024 + 1), b'{"version":1']:
            failed = subprocess.run([executable], cwd=directory, input=payload, capture_output=True, timeout=5)
            assert failed.returncode != 0
            assert b"private-fixture-marker" not in failed.stdout + failed.stderr
        assert not (root / "outside").exists()
    print("PASS: build/reopen/query, checksums, corruption fallback, malformed requests, path safety, frame limits")


if __name__ == "__main__":
    main()
