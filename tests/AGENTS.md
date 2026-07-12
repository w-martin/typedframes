## Test Guidelines
- Name tests `test_should_<expected_behavior>`
- Follow AAA pattern: `# arrange`, `# act`, `# assert` comments
- Use `unittest.TestCase` for all tests
- Unit tests in `tests/unit/`, integration tests in `tests/integration/`
- One test class per file, one class-under-test per test class
- No `if __name__ == "__main__"` blocks
- Minimize `patch` usage - prefer dependency injection
