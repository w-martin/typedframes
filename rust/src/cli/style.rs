//! ANSI escape sequences for coloured terminal output.

pub(crate) const RESET: &str = "\x1b[0m";
pub(crate) const BOLD: &str = "\x1b[1m";
pub(crate) const BOLD_RED: &str = "\x1b[1;31m";
pub(crate) const BOLD_GREEN: &str = "\x1b[1;32m";
pub(crate) const BOLD_YELLOW: &str = "\x1b[1;33m";
// Low-key informational text: the coverage summary and non-actionable suggestions.
pub(crate) const DIM: &str = "\x1b[2m";
