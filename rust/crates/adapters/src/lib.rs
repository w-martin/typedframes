//! The ports `typedframes_domain` and `typedframes_app` declare, implemented on the real system.

mod cache;
mod files;
mod git;

/// The worker count `TYPEDFRAMES_JOBS` asks for, if it is a positive number.
pub fn configured_workers() -> Option<usize> {
    std::env::var("TYPEDFRAMES_JOBS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
}

pub use cache::DiskIndexCache;
pub use files::OsFiles;
pub use git::{vcs_problem, GitWorkspace};
