"""Unit tests for the LSP server's diagnostic-translation helpers."""

import tempfile
import unittest
from pathlib import Path

from lsprotocol import types
from pygls.workspace import TextDocument

from typedframes.lsp.server import diagnostic_range, project_root, to_diagnostic

_SOURCE = 'import pandas as pd\n\nprint(df["missing"])\n'


def _error(**overrides: object) -> dict:
    """Build a checker error dict, defaulting to the anchor on `df` in `_SOURCE`."""
    error = {
        "line": 3,
        "col": 7,
        "code": "unknown-column",
        "message": "Column 'missing' does not exist",
        "severity": "error",
    }
    error.update(overrides)
    return error


class TestLspDiagnosticTranslation(unittest.TestCase):
    """Unit tests for the module-level translation functions in typedframes.lsp.server."""

    def setUp(self) -> None:
        """Build a document over a small source snippet with a known error anchor."""
        # arrange
        self.document = TextDocument(uri="file:///project/pipeline.py", source=_SOURCE)

    def test_should_convert_a_one_indexed_anchor_to_a_zero_indexed_range(self) -> None:
        """The checker's 1-indexed line/col becomes an LSP 0-indexed position."""
        # act
        result = diagnostic_range(_error(), self.document)

        # assert
        self.assertEqual(result.start, types.Position(line=2, character=6))

    def test_should_underline_the_identifier_at_the_anchor(self) -> None:
        """The range ends after the identifier that starts at the anchor."""
        # act
        result = diagnostic_range(_error(), self.document)

        # assert
        self.assertEqual(result.end, types.Position(line=2, character=8))

    def test_should_underline_one_character_when_the_anchor_is_not_an_identifier(self) -> None:
        """A non-identifier anchor still yields a non-empty range."""
        # arrange - column 10 is the opening quote of "missing"
        error = _error(col=10)

        # act
        result = diagnostic_range(error, self.document)

        # assert
        self.assertEqual((result.start.character, result.end.character), (9, 10))

    def test_should_collapse_to_the_document_end_when_the_anchor_is_past_it(self) -> None:
        """A stale anchor beyond the document does not raise, it lands past the last line."""
        # arrange
        error = _error(line=99, col=1)

        # act
        result = diagnostic_range(error, self.document)

        # assert
        self.assertEqual((result.start, result.end), (types.Position(line=3, character=0),) * 2)

    def test_should_clamp_a_zero_line_and_column_to_the_document_start(self) -> None:
        """Defensive clamping keeps a 0 anchor from becoming a negative LSP position."""
        # arrange
        error = _error(line=0, col=0)

        # act
        result = diagnostic_range(error, self.document)

        # assert
        self.assertEqual(result.start, types.Position(line=0, character=0))

    def test_should_count_characters_in_the_encoding_the_client_negotiated(self) -> None:
        """Positions past a non-ASCII prefix are converted from code points to UTF-16 units."""
        # arrange - an astral-plane emoji is one code point but two UTF-16 units
        document = TextDocument(uri="file:///project/pipeline.py", source='print("\U0001f600", df["missing"])\n')
        error = _error(line=1, col=12)

        # act
        result = diagnostic_range(error, document)

        # assert
        self.assertEqual((result.start.character, result.end.character), (12, 14))

    def test_should_map_each_checker_severity_to_its_lsp_counterpart(self) -> None:
        """error/warning/info become Error/Warning/Information."""
        # arrange
        severities = ["error", "warning", "info"]

        # act
        results = [to_diagnostic(_error(severity=severity), self.document).severity for severity in severities]

        # assert
        self.assertEqual(
            results,
            [
                types.DiagnosticSeverity.Error,
                types.DiagnosticSeverity.Warning,
                types.DiagnosticSeverity.Information,
            ],
        )

    def test_should_treat_an_unrecognised_severity_as_an_error(self) -> None:
        """An unknown severity is still reported rather than dropped."""
        # act
        result = to_diagnostic(_error(severity="catastrophe"), self.document)

        # assert
        self.assertEqual(result.severity, types.DiagnosticSeverity.Error)

    def test_should_carry_the_code_message_and_source_onto_the_diagnostic(self) -> None:
        """The diagnostic is attributable back to typedframes and its error code."""
        # act
        result = to_diagnostic(_error(), self.document)

        # assert
        self.assertEqual(
            (result.code, result.message, result.source),
            ("unknown-column", "Column 'missing' does not exist", "typedframes"),
        )

    def test_should_find_the_nearest_ancestor_with_a_pyproject(self) -> None:
        """The project root is the closest directory holding a pyproject.toml."""
        # arrange
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            (root / "pyproject.toml").write_text("[tool.typedframes]\n")
            nested = root / "src" / "package"
            nested.mkdir(parents=True)

            # act
            result = project_root(nested / "pipeline.py")

            # assert
            self.assertEqual(result, root)

    def test_should_fall_back_to_the_containing_directory_without_a_pyproject(self) -> None:
        """A file outside any project still gets a usable root."""
        # arrange
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp).resolve()

            # act
            result = project_root(directory / "pipeline.py")

            # assert
            self.assertEqual(result, directory)
