# Copilot Instructions for typedframes

## Test Guidelines
- Name tests `test_should_<expected_behavior>`
- Follow AAA pattern: `# arrange`, `# act`, `# assert` comments
- Use `unittest.TestCase` for all tests
- Unit tests: `tests/test_<module>.py`, Integration: `tests/test_integration.py`
- No `if __name__ == "__main__"` blocks
- Minimize `patch` usage - prefer dependency injection

## Code Style
- Type hints on all functions
- 120 char line limit
- Google-style docstrings
- Custom exceptions with descriptive messages

## Lint Ignore Policy
- Never add ignore rules without user approval
- Never skip bandit rules
- Exceptions must be general patterns, not case-by-case

## Commands
- `uv run inv build` - Build Rust linter (if source changed)
- `uv run inv test` - Tests with coverage (auto-builds)
- `uv run inv lint` - All linters
- `uv run inv all` - Full check suite

## Documentation Policy
- Never add future work or roadmap items without user approval
- Never add "Contributing" sections
- Check with user before suggesting planned features
