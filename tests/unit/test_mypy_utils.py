"""Unit tests for mypy plugin utility functions."""

import io
import unittest
from pathlib import Path
from unittest.mock import patch

from typedframes_lint.mypy import get_project_root, is_enabled


class TestUtilsUnit(unittest.TestCase):
    """Unit tests for utility functions."""

    def test_should_find_project_root(self) -> None:
        """Test that project root is found by looking for pyproject.toml."""
        # arrange
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch("pathlib.Path.is_file") as mock_is_file,
        ):
            # First call: current is file, pop to parent.
            # Then pyproject.toml exists in grandparent.
            mock_is_file.return_value = True
            mock_exists.side_effect = [False, True]

            # act
            root = get_project_root(Path("/a/b/c.py"))

            # assert
            self.assertEqual(root, Path("/a"))

        # Test case where it reaches root without finding pyproject.toml
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch("pathlib.Path.is_file") as mock_is_file,
        ):
            mock_is_file.return_value = False
            mock_exists.return_value = False

            # act
            root = get_project_root(Path("/"))

            # assert
            self.assertEqual(root, Path("/"))

    def test_should_check_enabled_status(self) -> None:
        """Test that is_enabled reads config correctly."""
        # arrange - test path (not real, mocked)
        test_path = Path("/fake/project")

        # Test missing config
        with patch("pathlib.Path.exists") as mock_exists:
            mock_exists.return_value = False
            # act/assert
            self.assertTrue(is_enabled(test_path))

        # Test enabled
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch.object(
                Path,
                "open",
                return_value=io.BytesIO(b"[tool.typedframes]\nenabled = true"),
            ),
        ):
            mock_exists.return_value = True
            # act/assert
            self.assertTrue(is_enabled(test_path))

        # Test disabled
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch.object(
                Path,
                "open",
                return_value=io.BytesIO(b"[tool.typedframes]\nenabled = false"),
            ),
        ):
            mock_exists.return_value = True
            # act/assert
            self.assertFalse(is_enabled(test_path))

        # Test exception
        with (
            patch("pathlib.Path.exists") as mock_exists,
            patch.object(Path, "open", side_effect=Exception()),
        ):
            mock_exists.return_value = True
            # act/assert
            self.assertTrue(is_enabled(test_path))
