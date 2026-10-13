"""Tests for seeing through a returned frame to what its callers read."""

from io import StringIO
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import patch

from typedframes.cli import main

_LOADERS = (
    "import pandas as pd\n\n\n"
    "def load_orders():\n"
    '    df = pd.read_csv("orders.csv")\n'
    "    return df\n\n\n"
    "def load_users():\n"
    '    df = pd.read_csv("users.csv")\n'
    "    return df\n\n\n"
    "def load_unused():\n"
    '    df = pd.read_csv("unused.csv")\n'
    "    return df\n"
)

_JOBS = (
    "from loaders import load_orders, load_unused, load_users\n\n\n"
    "def revenue():\n"
    "    orders = load_orders()\n"
    '    return orders["amount"].sum()\n\n\n'
    "def regions():\n"
    "    orders = load_orders()\n"
    '    return orders["region"]\n\n\n'
    "def users_passthrough():\n"
    "    users = load_users()\n"
    "    return users\n"
)


def _suggest(root: Path, *extra: str) -> str:
    """Run `suggest` on the project at `root` and return what it printed."""
    out = StringIO()
    with patch("sys.stdout", out), patch("sys.stderr", StringIO()):
        main(["suggest", str(root), "--no-cache", *extra])
    return out.getvalue()


class TestReturnedFrames(TestCase):
    """Whether a loader that returns its frame can be narrowed to what its callers read."""

    def _project(self, directory: str) -> Path:
        root = Path(directory)
        (root / "pyproject.toml").write_text("")
        (root / "loaders.py").write_text(_LOADERS)
        (root / "jobs.py").write_text(_JOBS)
        return root

    def test_should_narrow_a_returned_frame_to_the_columns_every_caller_reads(self) -> None:
        """Test that the callers' columns are suggested, as a review-tier change with a note."""
        # arrange
        with TemporaryDirectory() as directory:
            root = self._project(directory)

            # act
            out = _suggest(root)

            # assert
            self.assertIn('usecols ["amount", "region"]  [review]', out)
            self.assertIn("read by the callers of `load_orders` (2 calls)", out)
            self.assertIn("callers outside it, or reaching the function dynamically, can't be seen", out)

    def test_should_not_narrow_a_frame_when_a_caller_hands_the_result_on(self) -> None:
        """Test that one caller returning the frame leaves it withheld."""
        # arrange
        with TemporaryDirectory() as directory:
            root = self._project(directory)

            # act
            out = _suggest(root)

            # assert
            suggested, withheld = out.split("Not suggested:")
            self.assertNotIn("users.csv", suggested)
            self.assertEqual(2, withheld.count("returned (line"))

    def test_should_not_narrow_a_frame_nothing_in_the_project_calls(self) -> None:
        """Test that a function with no callers, which may be public API, stays withheld."""
        # arrange
        with TemporaryDirectory() as directory:
            root = self._project(directory)

            # act
            out = _suggest(root)

            # assert
            self.assertNotIn("unused.csv", out.split("Not suggested:")[0])

    def test_should_not_see_through_a_return_when_only_one_file_is_indexed(self) -> None:
        """Test that suggesting on a single file, whose callers are never indexed, narrows nothing."""
        # arrange
        with TemporaryDirectory() as directory:
            root = self._project(directory)

            # act
            out = _suggest(root / "loaders.py")

            # assert
            self.assertIn("0 suggestions", out)

    def test_should_not_see_through_a_return_without_the_whole_project_index(self) -> None:
        """Test that `--no-index` can't see callers, so nothing is narrowed."""
        # arrange
        with TemporaryDirectory() as directory:
            root = self._project(directory)

            # act
            out = _suggest(root, "--no-index")

            # assert
            self.assertIn("0 suggestions", out)

    def test_should_not_narrow_a_frame_a_caller_reaches_through_an_import_alias(self) -> None:
        """Test that `from loaders import load_orders as fetch` can't hide a caller's columns."""
        # arrange
        with TemporaryDirectory() as directory:
            root = self._project(directory)
            (root / "aliased.py").write_text(
                "from loaders import load_orders as fetch\n\n\n"
                "def other():\n"
                "    d = fetch()\n"
                '    return d["discount"]\n'
            )

            # act
            out = _suggest(root)

            # assert
            self.assertNotIn("orders.csv", out.split("Not suggested:")[0])

    def test_should_not_narrow_a_frame_a_caller_stores_in_a_module_level_name(self) -> None:
        """Test that a module-level result, which other modules can import and read, stays unknown."""
        # arrange
        with TemporaryDirectory() as directory:
            root = self._project(directory)
            (root / "shared.py").write_text(
                'from loaders import load_orders\n\nORDERS = load_orders()\nprint(ORDERS["amount"])\n'
            )

            # act
            out = _suggest(root)

            # assert
            self.assertNotIn("orders.csv", out.split("Not suggested:")[0])
