"""Exercises only disposable text fixtures and the bundled worker protocol."""
import json
import math
import os
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def frame(identifier, operation="probe", texts=None, version=1, **extra):
    return json.dumps(dict(version=version, id=identifier, operation=operation,
                           texts=[] if texts is None else texts, **extra)).encode() + b"\n"


def receive(process):
    deadline = time.monotonic() + 10
    data = bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while b"\n" not in data:
            remaining = deadline - time.monotonic()
            assert remaining > 0 and selector.select(remaining), "Worker must respond without stdin EOF"
            chunk = os.read(process.stdout.fileno(), 4096)
            assert chunk, "Worker exited before responding"
            data.extend(chunk)
            assert len(data) <= 262_145, "Response exceeds protocol limit"
    return json.loads(data)


def run(binary, payload):
    result = subprocess.run([binary], input=payload, capture_output=True, timeout=15)
    assert b"private-fixture-marker" not in result.stdout + result.stderr
    return result, [json.loads(line) for line in result.stdout.splitlines()]


def main():
    binary = sys.argv[1]
    process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        process.stdin.write(frame(1))
        process.stdin.flush()
        probe = receive(process)
        assert probe["id"] == 1 and probe["version"] == 1
        available = probe.get("error") != "modelUnavailable"
        if "--require-model" in sys.argv:
            assert available, "Native model positive control is required"
        if available:
            assert probe["model"]["identifier"] == "apple-contextual-en"
            process.stdin.write(frame(2, "embed", ["A local database migration fixture.", "A bread recipe fixture."]))
            process.stdin.flush()
            reply = receive(process)
            assert reply["id"] == 2 and "error" not in reply
            assert len(reply["vectors"]) == 2
            for vector in reply["vectors"]:
                assert len(vector) == reply["model"]["dimensions"]
                assert all(math.isfinite(value) for value in vector)
                assert abs(sum(value * value for value in vector) - 1) < 0.0001
        else:
            process.stdin.write(frame(2, "embed", ["A local fixture."]))
            process.stdin.flush()
            assert receive(process)["error"] == "modelUnavailable"
        process.kill()
        process.wait(timeout=2)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=2)
        process.stdin.close()
        process.stdout.close()
        process.stderr.close()

    with tempfile.TemporaryDirectory(prefix="blindspot-semantic-test-") as directory:
        forbidden = Path(directory) / "must-not-be-created"
        cases = [
            (frame(3, version=2), "unsupportedProtocol"),
            (frame(4, extra="private-fixture-marker"), "invalidRequest"),
            (frame(5, "execute_shell", [f"touch {forbidden}"]), "invalidRequest"),
            (frame(6, "embed", ["fixture"] * 9), "inputTooLarge"),
            (frame(7, "embed", ["x" * 4097]), "inputTooLarge"),
            (frame(8, "embed", ["x" * 4096] * 5), "inputTooLarge"),
            (frame(-1), "invalidRequest"),
            (b'{"private-fixture-marker":\n', "invalidRequest"),
            (frame(9, "embed", []), "invalidRequest"),
        ]
        result, replies = run(binary, b"".join(payload for payload, _ in cases) + frame(10))
        assert result.returncode == 0 and len(replies) == len(cases) + 1
        for reply, (_, error) in zip(replies, cases):
            assert reply["error"] == error
        assert replies[-1]["id"] == 10
        assert not forbidden.exists()
        for payload, expected in [(b"x" * 65_537 + b"\n", "inputTooLarge"), (b'{"id":11', "truncatedFrame")]:
            result, replies = run(binary, payload)
            assert result.returncode == 1 and replies[0]["error"] == expected
    print("Semantic worker: interactive framing, model " + ("inference" if available else "unavailable fallback")
          + ", 11 malformed/oversized cases, recovery, privacy and termination passed")


if __name__ == "__main__":
    main()
