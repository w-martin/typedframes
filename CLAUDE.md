# Claude Code Instructions for typedframes

## Test Style Guidelines

### Test Naming Convention
- Use `test_should_<expected_behavior>` naming pattern
- Example: `test_should_detect_missing_column`, `test_should_preserve_type_after_merge`

### Test Structure (AAA Pattern)
All tests must follow the Arrange-Act-Assert pattern:

```python
def test_should_do_something(self) -> None:
    """Describe what the test verifies."""
    # arrange
    sut = SystemUnderTest()
    expected = "expected_value"

    # act
    result = sut.do_something()

    # assert
    self.assertEqual(result, expected)
```

### Test Organization
- Use `unittest.TestCase` for all test classes
- Unit tests go in `tests/test_<module>.py`
- Integration tests go in `tests/test_integration.py`
- Keep unit and integration tests separate

### Mocking Guidelines
- Minimize use of `patch` - prefer dependency injection where possible
- When mocking is necessary, mock at the boundary (e.g., external services, file system)
- Avoid mocking internal implementation details

### Test File Structure
- No `if __name__ == "__main__"` blocks - pytest handles test discovery
- One test class per file matching the module under test
- Import the system under test (sut) from the main package

### Example Test

```python
"""Unit tests for Column class."""

import unittest

from typedframes import Column


class TestColumn(unittest.TestCase):
    """Unit tests for Column descriptor."""

    def test_should_create_column_with_type(self) -> None:
        """Test that Column can be created with a specific type."""
        # arrange
        expected_type = int

        # act
        sut = Column(type=expected_type)

        # assert
        self.assertEqual(sut.type, expected_type)
```

## Code Style
- Use type hints for all function signatures
- Follow PEP 8 with 120 character line limit
- Use Google-style docstrings
- Run `uv run inv lint` before committing

## Running Tests
```bash
uv run inv test      # Run all tests with coverage
uv run inv lint      # Run linters
uv run inv all       # Run all checks
```
