"""Running the `typedframes` command line: as the executable installed alongside this package, or in this process.

The in-process mode runs the same checker from the `typedframes._rust` extension module. It is for
environments where the Python interpreter may run code but no other executable may be started.
"""

import os
import shutil
import subprocess
import sys
from importlib import import_module
from pathlib import Path
from sysconfig import get_config_var, get_path, get_preferred_scheme
from types import ModuleType

IN_PROCESS_VARIABLE = "TYPEDFRAMES_INPROC"
_OFF_VALUES = frozenset({"", "0", "false", "no", "off"})


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


def _requested_in_process() -> bool:
    """Whether the environment asks for the checker to run inside this process."""
    return os.environ.get(IN_PROCESS_VARIABLE, "").strip().lower() not in _OFF_VALUES


def _extension() -> ModuleType:
    """The `typedframes._rust` extension module.

    Raises:
        BinaryNotFoundError: If the package was installed without it.
    """
    try:
        return import_module("typedframes._rust")
    except ImportError as e:
        msg = (
            "The typedframes extension module (typedframes._rust) was not found. "
            "Reinstall typedframes from a wheel, or build it from source with: uv run inv build"
        )
        raise BinaryNotFoundError(msg) from e


def _executable_missing() -> bool:
    try:
        find_binary()
    except BinaryNotFoundError:
        return True
    return False


def _in_process() -> bool:
    """Whether to run in this process: when asked to, or when the executable is missing but the extension is not."""
    if _requested_in_process():
        return True
    if not _executable_missing():
        return False
    try:
        _extension()
    except BinaryNotFoundError:
        return False
    return True


def run_binary(arguments: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    """Run the command line with `arguments`; with `capture`, its standard output is returned instead of shared.

    Raises:
        BinaryNotFoundError: If neither the executable nor the extension module is installed.
    """
    if _in_process():
        extension = _extension()
        if capture:
            status, stdout, stderr = extension.run_captured(["typedframes", *arguments])
            return subprocess.CompletedProcess(args=arguments, returncode=status, stdout=stdout, stderr=stderr)
        return subprocess.CompletedProcess(args=arguments, returncode=extension.main(["typedframes", *arguments]))
    return subprocess.run([str(find_binary()), *arguments], capture_output=capture, text=True, check=False)


def main() -> None:
    """Entry point for `python -m typedframes`: the command line, with this process's arguments.

    Where the platform allows it the executable takes this process's place, so no Python process
    is left waiting; Windows has no such call, and runs it as a child instead. Set `TYPEDFRAMES_INPROC=1`
    to run in this process instead of starting the executable.
    """
    if sys.platform == "win32" or _in_process():
        sys.exit(run_binary(sys.argv[1:]).returncode)
    binary = str(find_binary())
    os.execv(binary, [binary, *sys.argv[1:]])


def main_in_process() -> None:
    """Entry point for `typedframes-py`: always the checker inside this process, never the executable."""
    sys.exit(_extension().main(["typedframes", *sys.argv[1:]]))
