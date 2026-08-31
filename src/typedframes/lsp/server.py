"""Language Server Protocol server that publishes typedframes diagnostics to any editor.

The mypy plugin (`typedframes.mypy`) only reaches editors whose Python backend is
mypy. Pyright/Pylance -- the default in VS Code -- has no plugin mechanism, so the
only editor-agnostic way to surface column diagnostics inline is to speak LSP
directly. This module is that server: a thin translation layer over the same Rust
entry points the CLI and the mypy plugin already call
(`typedframes._rust_checker.check_file` / `build_project_index`), with no checking
logic of its own.

Scope is deliberately small: the server publishes diagnostics on open, save, and
close, and implements nothing else -- no hover, no completion, no code actions. It
does NOT re-check on `textDocument/didChange`, because the Rust `check_file` entry
point reads the file from disk; running it against an unsaved buffer would report
positions from stale text, which is worse than reporting nothing. Editors keep the
last published diagnostics until the next save, which is the same cadence the CLI
offers.
"""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

from lsprotocol import types
from pygls.lsp.server import LanguageServer
from pygls.uris import to_fs_path
from pygls.workspace import ServerTextPosition, ServerTextRange

from typedframes import __version__

if TYPE_CHECKING:
    from pygls.workspace import TextDocument

SERVER_NAME = "typedframes"

# The checker's own severities, in LSP terms. `info` is typedframes' low-key tier
# (see the CLI's --no-info), which maps to LSP's Information rather than Hint so it
# stays visible in editors that hide hints by default.
_SEVERITIES = {
    "error": types.DiagnosticSeverity.Error,
    "warning": types.DiagnosticSeverity.Warning,
    "info": types.DiagnosticSeverity.Information,
}

# The checker reports a single 1-indexed (line, col) anchor per diagnostic, not a
# span. Underlining the identifier that starts at that anchor (`df` in
# `df["missing"]`, `pl` in `pl.col("missing")`) gives editors something to squiggle
# without inventing an end position the checker never computed.
_IDENTIFIER = re.compile(r"\w+")


# The two Rust entry points the server needs, named the same way the CLI names its
# own alias for `check_file` (`_CheckFileFn`).
_CheckFileFn = Callable[[str, bytes | None], str]
_BuildIndexFn = Callable[[str], bytes]


def _rust_check_file(file_path: str, index_bytes: bytes | None) -> str:
    """Run the Rust checker on one file, returning its raw JSON result."""
    from typedframes._rust_checker import check_file

    return str(check_file(file_path, index_bytes))


def _rust_build_index(project_root: str) -> bytes:
    """Build the Rust cross-file project index for `project_root`."""
    from typedframes._rust_checker import build_project_index

    return build_project_index(project_root)


def project_root(path: Path) -> Path:
    """Find the project root for `path` by walking up to the nearest `pyproject.toml`.

    Matches how the mypy plugin resolves a root, so both integrations build the
    cross-file index over the same tree and agree on `[tool.typedframes]` config.
    Falls back to the file's own directory when nothing is found.
    """
    current = path.resolve().parent
    for parent in [current, *current.parents]:
        if (parent / "pyproject.toml").exists():
            return parent
    return current


def diagnostic_range(error: dict, document: TextDocument) -> types.Range:
    """Convert one checker error's 1-indexed anchor into an LSP range.

    LSP positions are 0-indexed, and their `character` is counted in the encoding
    the client negotiated, whereas the checker counts code points -- so the anchor is
    built as a `ServerTextRange` (pygls' name for a range in code points, indexable
    into `document.lines`) and converted by the document rather than emitted directly.
    """
    line = max(error["line"] - 1, 0)
    start = max(error["col"] - 1, 0)
    lines = document.lines
    text = lines[line] if line < len(lines) else ""
    match = _IDENTIFIER.match(text, start)
    end = match.end() if match else start + 1
    return document.range_to_client_units(
        ServerTextRange(
            start=ServerTextPosition(line=line, character=start),
            end=ServerTextPosition(line=line, character=end),
        )
    )


def to_diagnostic(error: dict, document: TextDocument) -> types.Diagnostic:
    """Translate one checker error into an LSP diagnostic."""
    return types.Diagnostic(
        range=diagnostic_range(error, document),
        message=error["message"],
        # An unrecognised severity is reported rather than dropped or raised on: a
        # new checker severity must not take the editor's diagnostics down with it.
        severity=_SEVERITIES.get(error["severity"], types.DiagnosticSeverity.Error),
        code=error["code"],
        source=SERVER_NAME,
    )


class TypedFramesLanguageServer(LanguageServer):
    """Language server that re-runs the Rust checker and publishes its diagnostics.

    The Rust entry points are injected rather than imported at call time so the
    server can be exercised in tests without the compiled extension, and so a
    missing extension is a normal (empty) result instead of a crashed server.
    """

    def __init__(
        self,
        check_file: _CheckFileFn = _rust_check_file,
        build_index: _BuildIndexFn = _rust_build_index,
    ) -> None:
        """Create a server bound to the given checker entry points."""
        super().__init__(SERVER_NAME, __version__)
        self._check_file = check_file
        self._build_index = build_index
        self._index_by_root: dict[str, bytes | None] = {}

    def index_bytes(self, root: Path) -> bytes | None:
        """Return the cross-file project index for `root`, building it once per root.

        Without it the checker cannot resolve a schema defined in another module, so
        an editor would report false `unknown-column` errors on perfectly valid code.
        """
        key = str(root)
        if key not in self._index_by_root:
            try:
                self._index_by_root[key] = self._build_index(key)
            except ImportError:
                self._index_by_root[key] = None
        return self._index_by_root[key]

    def clear_index_cache(self) -> None:
        """Drop every cached project index, so the next check sees the new sources."""
        self._index_by_root.clear()

    def diagnostics(self, path: Path, document: TextDocument) -> list[types.Diagnostic]:
        """Check one file on disk and return its diagnostics.

        Every failure mode is an empty list rather than an exception: a missing
        extension (`ImportError`), an unreadable file (`OSError`), or source that
        does not currently parse (`RuntimeError`, the checker's only other failure --
        see `_check_python_file` in the CLI). Mid-edit syntax errors are the common
        case here, and the editor's own Python backend already reports those.
        """
        try:
            raw = self._check_file(str(path), self.index_bytes(project_root(path)))
        except (ImportError, OSError, RuntimeError):
            return []
        errors: list[dict] = json.loads(raw)["errors"]
        return [to_diagnostic(error, document) for error in errors]

    def publish(self, uri: str) -> None:
        """Check the document at `uri` and publish the result to the client.

        Anything that isn't a `.py` path is ignored, which covers both notebooks
        (`.ipynb` needs the separate `check_notebook` entry point the CLI uses) and
        non-`file:` URIs such as an editor's unsaved `untitled:` buffers -- for which
        `to_fs_path` returns `None`, since there is no file on disk to check.
        """
        path = to_fs_path(uri)
        if path is None or not path.endswith(".py"):
            return
        document = self.workspace.get_text_document(uri)
        self.text_document_publish_diagnostics(
            types.PublishDiagnosticsParams(uri=uri, diagnostics=self.diagnostics(Path(path), document))
        )


def did_open(ls: TypedFramesLanguageServer, params: types.DidOpenTextDocumentParams) -> None:
    """Publish diagnostics for a newly opened document."""
    ls.publish(params.text_document.uri)


def did_save(ls: TypedFramesLanguageServer, _params: types.DidSaveTextDocumentParams) -> None:
    """Re-check every open document after a save.

    A save is the only moment the on-disk sources the checker reads can change, and
    the file that changed may be the one *defining* a schema -- so the stale index is
    dropped and all open buffers are refreshed, not just the saved one. Which file
    was saved therefore does not narrow the work, and the params go unused.

    Each document's own `uri` is republished, not the workspace's key for it: pygls
    stores documents under a percent-decoded key, and a client matches published
    diagnostics against the URI it sent. For a path with a space in it those two
    differ, and diagnostics keyed on the decoded form would land on no buffer.
    """
    ls.clear_index_cache()
    for document in list(ls.workspace.text_documents.values()):
        ls.publish(document.uri)


def did_close(ls: TypedFramesLanguageServer, params: types.DidCloseTextDocumentParams) -> None:
    """Clear diagnostics for a closed document, so nothing lingers in the editor."""
    ls.text_document_publish_diagnostics(types.PublishDiagnosticsParams(uri=params.text_document.uri, diagnostics=[]))


def create_server() -> TypedFramesLanguageServer:
    """Build the server with its three document handlers registered."""
    server = TypedFramesLanguageServer()
    server.feature(types.TEXT_DOCUMENT_DID_OPEN)(did_open)
    server.feature(types.TEXT_DOCUMENT_DID_SAVE)(did_save)
    server.feature(types.TEXT_DOCUMENT_DID_CLOSE)(did_close)
    return server


def main(server_factory: Callable[[], TypedFramesLanguageServer] = create_server) -> None:
    """Run the language server over stdio: the `typedframes-lsp` console script."""
    server_factory().start_io()
