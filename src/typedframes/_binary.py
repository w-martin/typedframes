"""Finding and running the `typedframes` executable that was installed alongside this package."""

import os
import shutil
import subprocess
import sys
from pathlib import Path
from sysconfig import get_config_var, get_path, get_preferred_scheme


class BinaryNotFoundError(FileNotFoundError):
    """Raised when the `typedframes` executable is not installed."""


def find_binary() -> Path:
    """The `typedframes` executable: next to the interpreter's scripts, then the user's, then on PATH.

    Raises:
        BinaryNotFoundError: If none of those hold it, e.g. the package was installed without it.
    """
    name = f"typedframes{get_config_var('EXE') or ''}"
    scripts = Path(get_path("scripts")) / name
    user_scripts = Path(get_path("scripts", scheme=get_preferred_scheme("user"))) / name
    # `pip install --target` puts scripts in a `bin` directory beside the package.
    target = Path(__file__).resolve().parents[1] / "bin" / name
    for candidate in (scripts, user_scripts, target):
        if candidate.is_file():
            return candidate
    on_path = shutil.which("typedframes")
    if on_path is not None:
        return Path(on_path)
    msg = (
        f"The typedframes executable was not found (looked for {scripts}). "
        "Reinstall typedframes from a wheel, or build it from source with: uv run inv build"
    )
    raise BinaryNotFoundError(msg)


def run_binary(arguments: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    """Run the executable with `arguments`; with `capture`, its output is returned instead of shared.

    Raises:
        BinaryNotFoundError: If the executable is not installed.
    """
    return subprocess.run([str(find_binary()), *arguments], capture_output=capture, text=True, check=False)


def main() -> None:
    """Entry point for `python -m typedframes`: the executable, with this process's arguments.

    Where the platform allows it the executable takes this process's place, so no Python process
    is left waiting; Windows has no such call, and runs it as a child instead.
    """
    if sys.platform == "win32":
        sys.exit(run_binary(sys.argv[1:]).returncode)
    binary = str(find_binary())
    os.execv(binary, [binary, *sys.argv[1:]])
