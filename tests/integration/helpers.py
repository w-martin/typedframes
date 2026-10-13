"""Run the installed `typedframes` executable the way the integration tests ask about a file.

`check_file(path)` is one file checked on its own; `check_file(path, index)` is that file checked as
part of the project the index was built for, so imports across files resolve.
"""

import json
from pathlib import Path
from typing import NamedTuple

from typedframes._binary import run_binary


class ProjectIndex(NamedTuple):
    """A project to check as a whole."""

    root: str


def build_project_index(root: str) -> ProjectIndex:
    """The project rooted at `root`."""
    return ProjectIndex(root)


def check_file(file_path: str, index: ProjectIndex | None = None) -> str:
    """The checker's JSON for one file: `{"errors": [...]}`."""
    if index is None:
        arguments = ["check", file_path, "--no-index", "--no-cache", "--output-format", "json"]
    else:
        arguments = ["check", index.root, "--no-cache", "--output-format", "json"]
    completed = run_binary(arguments, capture=True)
    if completed.returncode != 0:
        msg = f"typedframes check exited with status {completed.returncode}: {completed.stderr}"
        raise RuntimeError(msg)
    errors = json.loads(completed.stdout)["errors"]
    wanted = str(Path(file_path).resolve())
    return json.dumps({"errors": [error for error in errors if str(Path(error["file"]).resolve()) == wanted]})
