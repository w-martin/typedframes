# Claude Code Instructions for typedframes

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

### Agreed Exceptions (pyproject.toml)
- `D203`, `D212`: Conflict with D211/D213
- `COM812`: Conflicts with ruff formatter
- `PT009`, `PT027`: Using unittest.TestCase
- `PLC0415`: Lazy imports for optional dependencies (polars/pandas)
- `ANN401`: Any needed for pandas compatibility and Python protocols
- `tests/*`: `S101` (assert), `SLF001` (private access), `S603`/`S607` (subprocess)
- `tests/fixtures/*`: `T201` (print for test output)
- `examples/*`: `T201` (print), `INP001` (standalone scripts)
- `mypy.py`: `S603` (subprocess for linter execution)

## Commands
- `uv run inv build` - Build Rust linter (if source changed)
- `uv run inv test` - Tests with coverage (auto-builds)
- `uv run inv lint` - All linters
- `uv run inv all` - Full check suite

## Documentation Policy
- Never add future work, roadmap items, or collaboration suggestions without user approval
- Never add "Contributing" sections or invitations for external contributions
- Check with user before suggesting any planned features or improvements
