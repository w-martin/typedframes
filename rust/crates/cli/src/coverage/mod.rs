//! DataFrame schema coverage: the opt-in threshold gate and the `--coverage-detail` reports.
//!
//! Sites and events are the JSON objects the checker produced for each file, tagged with the
//! `file` they came from.

use super::config::{pattern_specificity, CoverageConfig, Glob};
use super::pathutil::relative_posix;
use super::pyfmt::{repr_str, round_half_even};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

mod explain;
mod pct;

pub(crate) use explain::*;
pub(crate) use pct::*;

/// One file's DataFrame coverage counts.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Tally {
    pub(crate) total: u64,
    pub(crate) typed: u64,
}

/// One threshold and the aggregate tally it is judged against.
#[derive(Debug, PartialEq)]
pub(crate) struct Bucket {
    /// The override glob this bucket came from, or `None` for the global threshold.
    pub(crate) label: Option<String>,
    pub(crate) threshold: f64,
    pub(crate) total: u64,
    pub(crate) typed: u64,
}

/// The raw per-file lists `--coverage-detail=explain` draws its report from.
#[derive(Default)]
pub(crate) struct Sources<'a> {
    pub(crate) all_dataframe_calls: &'a [Value],
    pub(crate) typed_sites: &'a [Value],
    pub(crate) untyped_sites: &'a [Value],
    pub(crate) leg_events: &'a [Value],
    pub(crate) untyped_site_usage: &'a [Value],
}

#[cfg(test)]
mod tests;
