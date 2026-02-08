# Copilot Instructions for typedframes

## Test Guidelines
- Name tests `test_should_<expected_behavior>`
- Follow AAA pattern: `# arrange`, `# act`, `# assert` comments
- Use `unittest.TestCase` for all tests
- Unit tests in `tests/unit/`, integration tests in `tests/integration/`
- One test class per file, one class-under-test per test class
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
- In `tasks.py`, don't prefix commands with `uv run` — invoke already runs inside the uv environment

## Instruction File Sync

- When updating this file (`.github/copilot-instructions.md`), also apply the same changes to `CLAUDE.md`

## Documentation Policy
- Never add future work or roadmap items without user approval
- Never add "Contributing" sections
- Check with user before suggesting planned features
