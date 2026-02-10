# Rust Pandas Linter internals

## Project Structure

Following Rust best practices (as of 2026), we maintain a clear separation between the library logic and the binary CLI.

- `src/lib.rs`: Contains the core logic of the linter. This allows the linter to be used as a dependency in other Rust projects or for integration testing.
- `src/main.rs`: A thin wrapper around the library that handles CLI arguments, file I/O, and stdout formatting.
- `tests/`: Integration tests that exercise the public API of the library.

## Unit Testing

Unit tests for private or small utility functions are kept in the same file as the code (e.g., at the bottom of `src/lib.rs` in a `mod tests`). This is standard practice in Rust for:
1. Accessing private members.
2. Keeping tests close to the implementation for better maintainability.
3. Reducing boilerplate for small, self-contained tests.

Larger unit tests or tests requiring complex setup may be moved to separate files within `src/` (e.g., `src/linter_tests.rs`) if `lib.rs` becomes too large.

## Binary vs Library

The `main.rs` file exists so that the tool can be compiled into an executable and called from the command line or from other tools like our `mypy` plugin. By putting the bulk of the logic in `lib.rs`, we ensure that:
1. We can write integration tests in `tests/*.rs`.
2. The logic is reusable.
3. Compilation is optimized.
