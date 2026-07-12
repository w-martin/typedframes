# Instructions

## Package Structure

- Import pattern: `from typedframes.pandas import PandasFrame`, `from typedframes.polars import PolarsFrame`

## Code Style
- Custom exceptions with descriptive messages

## Lint Ignore Policy
- Never add ignore rules without user approval
- Never skip bandit rules
- Exceptions must be general patterns, not case-by-case

## Commands

- `uv run inv build` - Build Rust checker in `rust/` (if source changed)
- `uv run inv test` - Tests with coverage (auto-builds)
- `uv run inv lint` - All linters
- `uv run inv all` - Full check suite
- In `tasks.py`, don't prefix commands with `uv run` — invoke already runs inside the uv environment

## Git Policy

- Never run git commands (commit, stash, push, checkout, etc.) without explicit user request

## Documentation Policy
- Never add future work, roadmap items, or collaboration suggestions without user approval
- Never add "Contributing" sections or invitations for external contributions
- Check with user before suggesting any planned features or improvements
