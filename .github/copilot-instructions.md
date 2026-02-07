# Copilot Instructions for typedframes

## Test Style Guidelines

### Test Naming
- Use `test_should_<expected_behavior>` naming pattern
- Example: `test_should_detect_missing_column`

### Test Structure (AAA Pattern)
All tests follow Arrange-Act-Assert:

```python
def test_should_do_something(self) -> None:
    """Describe what the test verifies."""
    # arrange
    sut = SystemUnderTest()

    # act
    result = sut.do_something()

    # assert
    self.assertEqual(result, expected)
```

### Test Organization
- Use `unittest.TestCase` for all tests
- Unit tests: `tests/test_<module>.py`
- Integration tests: `tests/test_integration.py`
- No `if __name__ == "__main__"` blocks

### Mocking
- Minimize `patch` usage - prefer dependency injection
- Mock at boundaries only (external services, file system)

## Code Style
- Type hints on all functions
- 120 char line limit
- Google-style docstrings
- Run `uv run inv lint` before commits

## Commands
```bash
uv run inv test      # Tests with coverage
uv run inv lint      # Linters
uv run inv all       # All checks
```
