"""Unit tests for the typedframes language server."""

import collections
import json
import tempfile
import textwrap
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import BinaryIO

from lsprotocol import types
from pygls.uris import from_fs_path
from pygls.workspace import TextDocument

from typedframes.lsp.server import (
    TypedFramesLanguageServer,
    _rust_build_index,
    _rust_check_file,
    create_server,
    did_close,
    did_open,
    did_save,
    main,
)

_ERROR = {
    "line": 3,
    "col": 7,
    "code": "unknown-column",
    "message": "Column 'missing' does not exist",
    "severity": "error",
}

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


def _checker(errors: list[dict]) -> Callable[..., str]:
    """Build a stand-in for the Rust `check_file` entry point that returns `errors`."""

    def check_file(*_args: object) -> str:
        return json.dumps({"errors": errors, "stats": {"dataframes_total": 1, "dataframes_typed": 1}})

    return check_file


def _raising(exception: Exception) -> Callable[..., str]:
    """Build a stand-in entry point that always raises `exception`."""

    def entry_point(*_args: object) -> str:
        raise exception

    return entry_point


def _recording_index_builder(builds: list[str]) -> Callable[[str], bytes]:
    """Build a stand-in `build_project_index` that records the roots it was asked for."""

    def build_index(project_root: str) -> bytes:
        builds.append(project_root)
        return b""

    return build_index


class _RecordingServer(TypedFramesLanguageServer):
    """A server whose published diagnostics are captured instead of sent to a client."""

    def __init__(
        self,
        check_file: Callable[..., str],
        build_index: Callable[[str], bytes] = lambda _root: b"",
    ) -> None:
        """Initialize the workspace the way a client's `initialize` request would."""
        super().__init__(check_file, build_index)
        self.published: list[types.PublishDiagnosticsParams] = []
        # pygls 2 implements its builtin handlers as generators, so that a
        # user-registered handler for the same method can be run part-way through.
        # Nothing here registers one, so the body runs straight to its return -- but
        # only once the generator is driven, hence exhausting it rather than calling it.
        collections.deque(
            self.protocol.lsp_initialize(types.InitializeParams(capabilities=types.ClientCapabilities())),
            maxlen=0,
        )

    def text_document_publish_diagnostics(self, params: types.PublishDiagnosticsParams) -> None:
        """Record the params instead of notifying a client that isn't there."""
        self.published.append(params)


def _document_item(uri: str, source: str) -> types.TextDocumentItem:
    """Build the document item a client sends with `didOpen`."""
    return types.TextDocumentItem(uri=uri, language_id="python", version=1, text=source)


class TestTypedFramesLanguageServer(unittest.TestCase):
    """Unit tests for TypedFramesLanguageServer and its document handlers."""

    def setUp(self) -> None:
        """Point every test at a `.py` document URI with a known error anchor."""
        # arrange
        self.uri = from_fs_path(str(Path("/project/pipeline.py")))
        self.source = 'import pandas as pd\n\nprint(df["missing"])\n'

    def test_should_publish_a_diagnostic_for_each_checker_error(self) -> None:
        """An open document's checker errors reach the client as diagnostics."""
        # arrange
        server = _RecordingServer(_checker([_ERROR]))
        item = _document_item(self.uri, self.source)
        server.workspace.put_text_document(item)

        # act
        did_open(server, types.DidOpenTextDocumentParams(text_document=item))

        # assert
        published = server.published[-1]
        self.assertEqual(
            (published.uri, len(published.diagnostics), published.diagnostics[0].code),
            (self.uri, 1, "unknown-column"),
        )

    def test_should_ignore_documents_that_are_not_python_files(self) -> None:
        """Notebooks and non-`file:` buffers are left alone."""
        # arrange
        server = _RecordingServer(_checker([_ERROR]))

        # act
        for uri in ("file:///project/analysis.ipynb", "untitled:Untitled-1"):
            server.publish(uri)

        # assert
        self.assertEqual(server.published, [])

    def test_should_publish_nothing_when_the_checker_cannot_read_the_file(self) -> None:
        """An unreadable or unparseable file clears diagnostics rather than crashing."""
        # arrange
        server = _RecordingServer(_raising(RuntimeError("invalid syntax")))
        server.workspace.put_text_document(_document_item(self.uri, self.source))

        # act
        server.publish(self.uri)

        # assert
        self.assertEqual([(p.uri, p.diagnostics) for p in server.published], [(self.uri, [])])

    def test_should_check_without_an_index_when_the_extension_is_missing(self) -> None:
        """A missing Rust extension degrades the index to None instead of raising."""
        # arrange
        server = _RecordingServer(_checker([]), _raising(ImportError("no extension")))

        # act
        result = server.index_bytes(Path("/project"))

        # assert
        self.assertIsNone(result)

    def test_should_build_the_project_index_only_once_per_root(self) -> None:
        """Repeated checks in one project reuse the cached index."""
        # arrange
        builds: list[str] = []
        server = _RecordingServer(_checker([]), _recording_index_builder(builds))

        # act
        server.index_bytes(Path("/project"))
        server.index_bytes(Path("/project"))

        # assert
        self.assertEqual(builds, [str(Path("/project"))])

    def test_should_rebuild_the_index_after_the_cache_is_cleared(self) -> None:
        """Clearing the cache makes the next check see the new sources."""
        # arrange
        builds: list[str] = []
        server = _RecordingServer(_checker([]), _recording_index_builder(builds))
        server.index_bytes(Path("/project"))

        # act
        server.clear_index_cache()
        server.index_bytes(Path("/project"))

        # assert
        self.assertEqual(builds, [str(Path("/project"))] * 2)

    def test_should_recheck_every_open_document_on_save(self) -> None:
        """A save may have changed a schema another open file depends on."""
        # arrange
        server = _RecordingServer(_checker([]))
        other = from_fs_path(str(Path("/project/schemas.py")))
        server.workspace.put_text_document(_document_item(self.uri, self.source))
        server.workspace.put_text_document(_document_item(other, self.source))

        # act
        did_save(
            server,
            types.DidSaveTextDocumentParams(text_document=types.TextDocumentIdentifier(uri=self.uri)),
        )

        # assert
        self.assertEqual(sorted(p.uri for p in server.published), sorted([self.uri, other]))

    def test_should_republish_on_save_under_the_uri_the_client_sent(self) -> None:
        """A percent-encoded path is republished as sent, not as pygls keys it internally."""
        # arrange - pygls stores documents under a percent-decoded key, so a path with a
        # space in it has a workspace key that no client would match diagnostics against
        server = _RecordingServer(_checker([]))
        uri = from_fs_path(str(Path("/my project/pipeline.py")))
        server.workspace.put_text_document(_document_item(uri, self.source))

        # act
        did_save(
            server,
            types.DidSaveTextDocumentParams(text_document=types.TextDocumentIdentifier(uri=uri)),
        )

        # assert
        self.assertEqual([p.uri for p in server.published], ["file:///my%20project/pipeline.py"])

    def test_should_clear_diagnostics_when_a_document_is_closed(self) -> None:
        """Closing a file removes its squiggles from the editor."""
        # arrange
        server = _RecordingServer(_checker([_ERROR]))

        # act
        did_close(
            server,
            types.DidCloseTextDocumentParams(text_document=types.TextDocumentIdentifier(uri=self.uri)),
        )

        # assert
        self.assertEqual([(p.uri, p.diagnostics) for p in server.published], [(self.uri, [])])

    def test_should_register_the_three_document_handlers(self) -> None:
        """The server advertises open, save and close, and nothing else."""
        # act
        server = create_server()

        # assert
        self.assertEqual(
            sorted(server.protocol.fm.features),
            sorted(
                [
                    types.TEXT_DOCUMENT_DID_OPEN,
                    types.TEXT_DOCUMENT_DID_SAVE,
                    types.TEXT_DOCUMENT_DID_CLOSE,
                ]
            ),
        )

    def test_should_start_the_server_over_stdio(self) -> None:
        """`main` builds a server and hands it the stdio transport."""
        # arrange
        started: list[tuple] = []

        class _StdioRecordingServer(TypedFramesLanguageServer):
            """A server that records the transport it was started on."""

            def start_io(self, stdin: BinaryIO | None = None, stdout: BinaryIO | None = None) -> None:
                """Record the streams it was started on instead of blocking on stdin."""
                started.append((stdin, stdout))

        # act
        main(server_factory=_StdioRecordingServer)

        # assert
        self.assertEqual(started, [(None, None)])

    def test_should_report_real_errors_through_the_rust_entry_points(self) -> None:
        """The default entry points wire the server to the compiled checker."""
        # arrange
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            (root / "pyproject.toml").write_text(_PYPROJECT)
            target = root / "pipeline.py"
            target.write_text(_PIPELINE)
            server = TypedFramesLanguageServer(_rust_check_file, _rust_build_index)
            document = TextDocument(uri=from_fs_path(str(target)), source=_PIPELINE)

            # act
            diagnostics = server.diagnostics(target, document)

            # assert
            self.assertEqual([d.code for d in diagnostics], ["unknown-column"])
