"""The executable and the in-process extension module must give the same answers."""

import os
from pathlib import Path
from unittest import TestCase
from unittest.mock import patch

from typedframes._binary import IN_PROCESS_VARIABLE, run_binary


class TestInProcessMatchesExecutable(TestCase):
    """Both ways of running the command line, on the same input."""

    def _run(self, arguments: list[str], *, in_process: bool) -> tuple[int, str, str]:
        environment = {key: value for key, value in os.environ.items() if key != IN_PROCESS_VARIABLE}
        if in_process:
            environment[IN_PROCESS_VARIABLE] = "1"
        with patch.dict(os.environ, environment, clear=True):
            completed = run_binary(arguments, capture=True)
        return completed.returncode, completed.stdout, completed.stderr

    def test_should_report_the_same_json_diagnostics(self) -> None:
        """Test that `check --output-format json` is byte-for-byte the same."""
        # arrange
        example = str(Path("examples/features/typedframes_example.py").absolute())
        arguments = ["check", example, "--no-cache", "--output-format", "json"]

        # act
        executable = self._run(arguments, in_process=False)
        extension = self._run(arguments, in_process=True)

        # assert
        self.assertEqual(executable, extension)
        self.assertIn("wrong_column", extension[1])

    def test_should_report_the_same_status_for_a_bad_command_line(self) -> None:
        """Test that a usage error is the same status in both."""
        # act
        executable = self._run(["no-such-command"], in_process=False)
        extension = self._run(["no-such-command"], in_process=True)

        # assert
        self.assertEqual(executable[0], extension[0])
        self.assertEqual(2, extension[0])

    def test_should_report_the_same_message_for_a_path_that_does_not_exist(self) -> None:
        """Test that the reason for a failed `check` reaches the caller in both."""
        # arrange
        arguments = ["check", "/no/such/path/anywhere", "--no-cache", "--output-format", "json"]

        # act
        executable = self._run(arguments, in_process=False)
        extension = self._run(arguments, in_process=True)

        # assert
        self.assertEqual(executable, extension)
        self.assertIn("path does not exist", extension[2])
