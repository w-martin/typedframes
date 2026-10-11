"""Tests for how the benchmark script summarises and prints timings."""

from statistics import stdev
from unittest import TestCase
from unittest.mock import patch

from benchmarks.benchmark_checkers import RUNS, BenchmarkResult, _format_cell, format_stat, generate_markdown_table


def _result(times: list[int], *, success: bool = True) -> BenchmarkResult:
    return BenchmarkResult(name="tool", description="what", version="1.0", times=times, success=success)


class TestBenchmarkStats(TestCase):
    """Median, spread and the one-unit `median ± std` cell."""

    def test_should_report_the_median_of_the_runs(self) -> None:
        """Test that the headline number is the middle run."""
        # arrange
        sut = _result([50, 10, 30, 20, 40])

        # act / assert
        self.assertEqual(30, sut.median)

    def test_should_not_let_one_slow_run_move_the_median(self) -> None:
        """Test that an outlier shifts the spread but not the median."""
        # arrange
        steady = _result([100, 101, 102, 103, 104])
        disturbed = _result([100, 101, 102, 103, 5000])

        # act / assert
        self.assertEqual(102, steady.median)
        self.assertEqual(steady.median, disturbed.median)
        self.assertGreater(disturbed.std, 100 * steady.std)

    def test_should_report_the_sample_standard_deviation_of_every_run(self) -> None:
        """Test that nothing is discarded before the spread is computed."""
        # arrange
        times = [100, 110, 90, 105, 5000]

        # act
        sut = _result(times)

        # assert
        self.assertEqual(stdev(times), sut.std)

    def test_should_report_no_spread_below_two_runs(self) -> None:
        """Test the guards for no runs and a single run."""
        # arrange / act / assert
        self.assertEqual(0.0, _result([]).median)
        self.assertEqual(0.0, _result([]).std)
        self.assertEqual(7, _result([7]).median)
        self.assertEqual(0.0, _result([7]).std)

    def test_should_format_milliseconds_with_one_decimal(self) -> None:
        """Test the unit used below one second."""
        # arrange / act / assert
        self.assertEqual("45.2 ± 3.1 ms", format_stat(45_200_000, 3_100_000))
        self.assertEqual("212.0 ± 2.4 ms", format_stat(212_000_000, 2_400_000))

    def test_should_format_seconds_with_two_decimals(self) -> None:
        """Test the unit used from one second up."""
        # arrange / act / assert
        self.assertEqual("1.95 ± 0.08 s", format_stat(1_950_000_000, 77_000_000))

    def test_should_format_microseconds_below_a_millisecond(self) -> None:
        """Test the unit used for very fast results."""
        # arrange / act / assert
        self.assertEqual("800 ± 41 µs", format_stat(800_000, 41_000))

    def test_should_show_both_numbers_in_the_unit_of_the_median(self) -> None:
        """Test that a small spread isn't switched to a smaller unit than its median."""
        # arrange / act
        cell = format_stat(1_200_000_000, 40_000_000)

        # assert
        self.assertEqual("1.20 ± 0.04 s", cell)

    def test_should_not_mention_the_interquartile_range_in_a_cell(self) -> None:
        """Test that cells show only median and standard deviation."""
        # arrange
        sut = _result([40_000_000, 44_000_000, 45_000_000, 46_000_000, 50_000_000])

        # act
        cell = _format_cell(sut)

        # assert
        self.assertNotIn("IQR", cell)
        self.assertTrue(cell.startswith("45.0 ± "))
        self.assertTrue(cell.endswith(" ms"))

    def test_should_caption_the_table_with_the_run_count_and_the_spread(self) -> None:
        """Test the line the README table is introduced with."""
        # arrange
        results = {"tool": {"proj": _result([45_000_000] * RUNS)}}

        # act
        with patch("benchmarks.benchmark_checkers.get_machine_info", return_value="machine"):
            table = generate_markdown_table(results, {"tool": ("1.0", "what")}, [("proj", 3)])

        # assert
        self.assertIn(f"median of {RUNS} runs ± one standard deviation", table.splitlines()[0])
        self.assertIn("| tool | 1.0 | what | 45.0 ± 0.0 ms |", table)
