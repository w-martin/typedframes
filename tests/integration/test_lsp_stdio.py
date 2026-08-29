"""Integration tests driving the language server as a real editor client would.

Speaks the wire protocol (Content-Length framed JSON-RPC over stdio) against
`python -m typedframes.lsp`, so the entry point, the pygls plumbing and the Rust
checker are all exercised together rather than stubbed.
"""

import json
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from typing import IO

import typedframes.lsp.__main__

_TIMEOUT_SECONDS = 60
_MAX_MESSAGES = 10

_PYPROJECT = "[tool.typedframes]\nenabled = true\n"

_PIPELINE = textwrap.dedent(
    """\
    from typing import Annotated

    import pandas as pd

    from typedframes import BaseSchema, Column


    class UserSchema(BaseSchema):
        user_id = Column(type=int)


    df: Annotated[pd.DataFrame, UserSchema] = pd.DataFrame({"user_id": [1]})
    print(df["missing"])
    """
)


def _frame(payload: dict) -> bytes:
    """Wrap a JSON-RPC payload in the LSP base protocol's Content-Length header."""
    body = json.dumps(payload).encode()
    return b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body


def _read_message(stream: IO[bytes]) -> dict:
    """Read one Content-Length framed JSON-RPC message from `stream`."""
    length = 0
    while True:
        line = stream.readline()
        if line in (b"\r\n", b"\n", b""):
            break
        name, _, value = line.decode().partition(":")
        if name.strip().lower() == "content-length":
            length = int(value.strip())
    return json.loads(stream.read(length))


def _read_until_diagnostics(stream: IO[bytes]) -> tuple[dict, dict]:
    """Read messages until the first publishDiagnostics, returning it and the initialize result."""
    initialize_result: dict = {}
    for _ in range(_MAX_MESSAGES):
        message = _read_message(stream)
        if "result" in message:
            initialize_result = message
        if message.get("method") == "textDocument/publishDiagnostics":
            return initialize_result, message
    msg = "server never published diagnostics"
    raise AssertionError(msg)


class TestLanguageServerOverStdio(unittest.TestCase):
    """End-to-end tests for `python -m typedframes.lsp`."""

    def test_should_expose_main_as_the_module_entry_point(self) -> None:
        """`python -m typedframes.lsp` resolves to the same entry point as the console script."""
        # act
        entry_point = typedframes.lsp.__main__.main

        # assert
        self.assertIs(entry_point, typedframes.lsp.main)

    def test_should_publish_diagnostics_for_an_opened_file(self) -> None:
        """A didOpen on a file with a bad column yields a publishDiagnostics notification."""
        # arrange
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            (root / "pyproject.toml").write_text(_PYPROJECT)
            target = root / "pipeline.py"
            target.write_text(_PIPELINE)

            process = subprocess.Popen(
                [sys.executable, "-m", "typedframes.lsp"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                cwd=str(root),
            )
            self.addCleanup(process.kill)
            stdin, stdout = process.stdin, process.stdout
            assert stdin is not None
            assert stdout is not None

            # act
            stdin.write(
                _frame(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {"processId": None, "rootUri": root.as_uri(), "capabilities": {}},
                    }
                )
            )
            stdin.write(_frame({"jsonrpc": "2.0", "method": "initialized", "params": {}}))
            stdin.write(
                _frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "textDocument/didOpen",
                        "params": {
                            "textDocument": {
                                "uri": target.as_uri(),
                                "languageId": "python",
                                "version": 1,
                                "text": _PIPELINE,
                            }
                        },
                    }
                )
            )
            stdin.flush()
            initialize_result, notification = _read_until_diagnostics(stdout)

            stdin.write(_frame({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}))
            stdin.write(_frame({"jsonrpc": "2.0", "method": "exit", "params": {}}))
            stdin.flush()
            process.wait(timeout=_TIMEOUT_SECONDS)

            # assert
            sync = initialize_result["result"]["capabilities"]["textDocumentSync"]
            diagnostics = notification["params"]["diagnostics"]
            self.assertEqual(
                (
                    sync["openClose"],
                    sync["save"],
                    notification["params"]["uri"],
                    [(d["code"], d["severity"], d["source"], d["range"]["start"]) for d in diagnostics],
                ),
                (
                    True,
                    True,
                    target.as_uri(),
                    [("unknown-column", 1, "typedframes", {"line": 12, "character": 6})],
                ),
            )
