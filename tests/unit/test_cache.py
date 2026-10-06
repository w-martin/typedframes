"""Tests for where the index cache lives and the `typedframes cache` commands."""

from io import StringIO
from os import environ
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import patch

from typedframes import _cache
from typedframes.cli import main


class TestCacheLocation(TestCase):
    """Where the cache directory is, and how each way of setting it ranks."""

    def test_should_use_the_macos_user_cache_dir_on_darwin(self) -> None:
        """Test the default on macOS."""
        # act
        with patch("typedframes._cache.platform", "darwin"):
            result = _cache.default_cache_dir()

        # assert
        self.assertEqual(result, Path.home() / "Library" / "Caches" / "typedframes")

    def test_should_honour_xdg_cache_home_on_linux(self) -> None:
        """Test that XDG_CACHE_HOME wins over ~/.cache."""
        # act
        with patch("typedframes._cache.platform", "linux"), patch.dict(environ, {"XDG_CACHE_HOME": "/xdg"}):
            result = _cache.default_cache_dir()

        # assert
        self.assertEqual(result, Path("/xdg/typedframes"))

    def test_should_fall_back_to_dot_cache_on_linux(self) -> None:
        """Test the default when XDG_CACHE_HOME is unset."""
        # act
        with patch("typedframes._cache.platform", "linux"), patch.dict(environ, {}, clear=True):
            result = _cache.default_cache_dir()

        # assert
        self.assertEqual(result, Path.home() / ".cache" / "typedframes")

    def test_should_use_local_app_data_on_windows(self) -> None:
        """Test the default on Windows, with and without LOCALAPPDATA."""
        # act
        with patch("typedframes._cache.platform", "win32"), patch.dict(environ, {"LOCALAPPDATA": "/local"}):
            with_var = _cache.default_cache_dir()
        with patch("typedframes._cache.platform", "win32"), patch.dict(environ, {}, clear=True):
            without_var = _cache.default_cache_dir()

        # assert
        self.assertEqual(with_var, Path("/local/typedframes/Cache"))
        self.assertEqual(without_var, Path.home() / "AppData" / "Local" / "typedframes" / "Cache")

    def test_should_resolve_nothing_when_caching_is_disabled(self) -> None:
        """Test that --no-cache beats every other setting."""
        # act
        with patch.dict(environ, {_cache.ENV_VAR: "/from/env"}):
            result = _cache.resolve_cache_dir(Path(), cli_value=Path("/flag"), disabled=True)

        # assert
        self.assertIsNone(result)

    def test_should_prefer_the_flag_then_the_environment_then_the_config_then_the_default(self) -> None:
        """Test the documented order of precedence."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir).resolve()
            (root / "pyproject.toml").write_text('[tool.typedframes]\ncache_dir = "from-config"\n')
            flag = root / "from-flag"

            # act
            with patch.dict(environ, {_cache.ENV_VAR: str(root / "from-env")}):
                by_flag = _cache.resolve_cache_dir(root, cli_value=flag)
                by_env = _cache.resolve_cache_dir(root)
            with patch.dict(environ, {}, clear=True):
                by_config = _cache.resolve_cache_dir(root)
                (root / "pyproject.toml").write_text("")
                by_default = _cache.resolve_cache_dir(root)

            # assert
            self.assertEqual(by_flag, flag)
            self.assertEqual(by_env, root / "from-env")
            self.assertEqual(by_config, root / "from-config")
            self.assertEqual(by_default, _cache.default_cache_dir())

    def test_should_use_the_default_when_there_is_no_project_to_read_config_from(self) -> None:
        """Test the per-user default for a path outside any project, with nothing else set."""
        # act
        with patch.dict(environ, {}, clear=True), patch("typedframes._cache.project_root_for", return_value=None):
            result = _cache.locate_cache_dir(Path("/no/project"))

        # assert
        self.assertEqual(result, _cache.default_cache_dir())

    def test_should_read_an_absolute_config_value_and_ignore_a_bad_one(self) -> None:
        """Test an absolute `cache_dir`, a non-string one, an empty one and unreadable TOML."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            pyproject = root / "pyproject.toml"

            # act / assert
            pyproject.write_text('[tool.typedframes]\ncache_dir = "/abs/cache"\n')
            self.assertEqual(_cache.configured_cache_dir(root), Path("/abs/cache"))
            for text in ("[tool.typedframes]\ncache_dir = 3\n", '[tool.typedframes]\ncache_dir = ""\n', "= not toml"):
                pyproject.write_text(text)
                self.assertIsNone(_cache.configured_cache_dir(root))
            pyproject.unlink()
            self.assertIsNone(_cache.configured_cache_dir(root))

    def test_should_find_the_project_root_above_a_file(self) -> None:
        """Test walking up to the nearest pyproject.toml, and finding none."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir).resolve()
            (root / "pyproject.toml").write_text("")
            nested = root / "a" / "b"
            nested.mkdir(parents=True)

            # act / assert
            self.assertEqual(_cache.project_root_for(nested), root)
            self.assertEqual(_cache.project_root_for(nested / "mod.py"), root)
        with patch("pathlib.Path.is_file", return_value=False):
            self.assertIsNone(_cache.project_root_for(Path("/nowhere")))


class TestCacheHousekeeping(TestCase):
    """Listing the cache's files and the .gitignore entry."""

    def test_should_list_only_the_files_the_tool_wrote(self) -> None:
        """Test that unrelated files in the directory are never listed."""
        with TemporaryDirectory() as tmpdir:
            cache = Path(tmpdir)
            (cache / "index-abc.bin").write_bytes(b"x")
            (cache / "index-abc.tmp123").write_bytes(b"x")
            (cache / "notes.txt").write_text("mine")

            # act
            files = _cache.cache_files(cache)

            # assert
            self.assertEqual([f.name for f in files], ["index-abc.bin", "index-abc.tmp123"])
            self.assertEqual(_cache.cache_files(cache / "missing"), [])

    def test_should_give_a_gitignore_entry_only_for_a_directory_inside_the_project(self) -> None:
        """Test the entry for an in-project cache and none for one outside."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            # act / assert
            self.assertEqual(_cache.gitignore_entry(root / ".typedframes_cache", root), "/.typedframes_cache/")
            self.assertIsNone(_cache.gitignore_entry(Path("/elsewhere/cache"), root))

    def test_should_not_ignore_the_whole_project_when_the_cache_is_the_project_root(self) -> None:
        """Test that a cache directory equal to the root yields no gitignore entry."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            self.assertIsNone(_cache.gitignore_entry(root, root))
            self.assertIsNone(_cache.gitignore_entry(root / ".", root))

    def test_should_keep_bytes_that_are_not_utf8_when_appending_to_gitignore(self) -> None:
        """Test that a gitignore with Latin-1 text is extended, not rejected or rewritten."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / ".gitignore").write_bytes(b"# caf\xe9\nbuild\n")

            self.assertTrue(_cache.add_to_gitignore(root, "/c/"))

            self.assertEqual((root / ".gitignore").read_bytes(), b"# caf\xe9\nbuild\n/c/\n")

    def test_should_append_to_gitignore_once(self) -> None:
        """Test creating, appending to (with a newline fix-up), and not duplicating."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            # act / assert
            self.assertTrue(_cache.add_to_gitignore(root, "/c/"))
            self.assertEqual((root / ".gitignore").read_text(), "/c/\n")
            self.assertFalse(_cache.add_to_gitignore(root, "/c/"))
            (root / ".gitignore").write_text("build")
            self.assertTrue(_cache.add_to_gitignore(root, "/c/"))
            self.assertEqual((root / ".gitignore").read_text(), "build\n/c/\n")
            (root / ".gitignore").write_text("c\n")
            self.assertFalse(_cache.add_to_gitignore(root, "/c/"))


class TestCacheCommand(TestCase):
    """`typedframes cache ...` and the cache flags on `check`."""

    def _run(self, argv: list[str]) -> str:
        captured = StringIO()
        with patch("sys.stdout", captured):
            main(argv)
        return captured.getvalue()

    def test_should_print_the_cache_dir(self) -> None:
        """Test `cache path` with an explicit directory."""
        with TemporaryDirectory() as tmpdir:
            # act
            output = self._run(["cache", "path", "--cache-dir", tmpdir])

            # assert
            self.assertEqual(output.strip(), str(Path(tmpdir).resolve()))

    def test_should_clear_only_cache_files(self) -> None:
        """Test that `cache clear` removes the index files and leaves anything else alone."""
        with TemporaryDirectory() as tmpdir:
            cache = Path(tmpdir)
            (cache / "index-1.bin").write_bytes(b"x" * 2048)
            (cache / "keep.txt").write_text("mine")

            # act
            output = self._run(["cache", "clear", "--cache-dir", tmpdir])

            # assert
            self.assertIn("Removed 1 cache file (2 KiB)", output)
            self.assertFalse((cache / "index-1.bin").exists())
            self.assertTrue((cache / "keep.txt").exists())

    def test_should_gitignore_a_cache_dir_inside_the_project(self) -> None:
        """Test `cache gitignore` writes the entry, and says so when it is already there."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text("")

            # act
            first = self._run(["cache", "gitignore", str(root), "--cache-dir", str(root / ".tf_cache")])
            second = self._run(["cache", "gitignore", str(root), "--cache-dir", str(root / ".tf_cache")])

            # assert
            self.assertIn("Added", first)
            self.assertIn("Already in", second)
            self.assertEqual((root / ".gitignore").read_text(), "/.tf_cache/\n")

    def test_should_explain_when_the_cache_dir_is_outside_the_project(self) -> None:
        """Test that `cache gitignore` writes nothing and points at --cache-dir for an outside cache."""
        with TemporaryDirectory() as tmpdir, TemporaryDirectory() as elsewhere:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text("")

            # act
            output = self._run(["cache", "gitignore", str(root), "--cache-dir", elsewhere])

            # assert
            self.assertIn("outside the project", output)
            self.assertIn("--cache-dir .typedframes_cache", output)
            self.assertFalse((root / ".gitignore").exists())

    def test_should_refuse_to_gitignore_a_cache_that_is_the_project_root(self) -> None:
        """Test that `cache gitignore` never ignores the whole project."""
        with TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            (root / "pyproject.toml").write_text("")

            # act
            output = self._run(["cache", "gitignore", str(root), "--cache-dir", str(root)])

            # assert
            self.assertIn("is the project root", output)
            self.assertFalse((root / ".gitignore").exists())

    def test_should_explain_when_there_is_no_project(self) -> None:
        """Test `cache gitignore` outside any project."""
        with TemporaryDirectory() as tmpdir:
            # act
            with patch("typedframes.cli.project_root_for", return_value=None):
                output = self._run(["cache", "gitignore", tmpdir, "--cache-dir", tmpdir])

            # assert
            self.assertIn("outside the project", output)

    def test_should_write_a_cache_when_checking_and_not_with_no_cache(self) -> None:
        """Test that `check` fills the cache dir, and `--no-cache` leaves it empty."""
        with TemporaryDirectory() as project, TemporaryDirectory() as cache:
            (Path(project) / "pyproject.toml").write_text("")
            (Path(project) / "a.py").write_text("x = 1\n")

            # act
            with patch("sys.stdout", StringIO()):
                main(["check", project, "--no-cache", "--cache-dir", cache])
            skipped = list(Path(cache).iterdir())
            with patch("sys.stdout", StringIO()):
                main(["check", project, "--cache-dir", cache])
            written = list(Path(cache).iterdir())

            # assert
            self.assertEqual(skipped, [])
            self.assertEqual(len(written), 1)
            self.assertTrue(written[0].name.startswith("index-"))
