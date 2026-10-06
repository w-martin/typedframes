"""Shared test configuration."""

from collections.abc import Iterator
from os import environ
from tempfile import TemporaryDirectory

import pytest


@pytest.fixture(autouse=True, scope="session")
def _isolated_index_cache() -> Iterator[None]:
    """Keep the on-disk index cache out of the real per-user cache directory.

    Tests run the CLI over throwaway projects; without this each one would leave a cache
    file behind in the developer's own cache directory.
    """
    with TemporaryDirectory() as cache_dir:
        previous = environ.get("TYPEDFRAMES_CACHE_DIR")
        environ["TYPEDFRAMES_CACHE_DIR"] = cache_dir
        try:
            yield
        finally:
            if previous is None:
                environ.pop("TYPEDFRAMES_CACHE_DIR", None)
            else:
                environ["TYPEDFRAMES_CACHE_DIR"] = previous
