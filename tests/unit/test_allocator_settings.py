"""Tests for the allocator settings the command line applies before the extension loads."""

from os import environ
from subprocess import run
from sys import executable
from unittest import TestCase

_VARIABLE = "_RJEM_MALLOC_CONF"
_PRINT = f"import os, typedframes.cli; print(os.environ.get({_VARIABLE!r}))"


def _value_seen_by_a_fresh_cli(extra: dict[str, str]) -> str:
    """Import the CLI in a new interpreter with `extra` set, and return the allocator setting it ends up with."""
    env = {name: value for name, value in environ.items() if name != _VARIABLE}
    env.update(extra)
    result = run([executable, "-I", "-c", _PRINT], capture_output=True, text=True, check=True, env=env)
    return result.stdout.strip()


class TestAllocatorSettings(TestCase):
    """The CLI tunes jemalloc for a short, many-threaded run, unless the user already did."""

    def test_should_set_fewer_arenas_and_eager_release_by_default(self) -> None:
        """Test that importing the CLI sets the allocator tuning when none is set."""
        # arrange
        extra: dict[str, str] = {}

        # act
        value = _value_seen_by_a_fresh_cli(extra)

        # assert
        self.assertEqual("narenas:4,dirty_decay_ms:0,muzzy_decay_ms:0", value)

    def test_should_keep_a_setting_the_user_already_made(self) -> None:
        """Test that the CLI does not overwrite the user's own allocator configuration."""
        # arrange
        extra = {_VARIABLE: "narenas:1"}

        # act
        value = _value_seen_by_a_fresh_cli(extra)

        # assert
        self.assertEqual("narenas:1", value)
