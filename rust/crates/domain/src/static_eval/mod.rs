//! Static evaluation of the values column lists, dicts and strings are built from.
//!
//! "Statically resolvable" means the value does not depend on IO, randomness, time, the
//! environment, function arguments, or any other runtime state. This module evaluates
//! such expressions (`Evaluator`), works out which names refer to one fixed value
//! (`Analysis`), and folds the results back into the AST as literals (`fold_constants`)
//! so every consumer in the checker sees a literal instead of a variable.

use crate::constants::{LOAD_FUNCTIONS, LOAD_MODULES};
use crate::fast_hash::{FastMap, FastSet};
use ruff_python_ast::visitor::transformer::{self as ast_transformer, Transformer};
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{
    self as ast, BoolOp, CmpOp, Expr, ExprContext, ModModule, Operator, UnaryOp,
};
use ruff_text_size::{Ranged, TextRange, TextSize};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod analysis;
mod builtins;
mod class_index;
mod collector;
mod collector_exprs;
mod collector_stmts;
mod dataclasses_enums;
mod evaluator;
mod exports;
mod folding;
mod foreign;
mod groups;
mod methods;
mod operations;
mod resolution;
mod resolver;
mod scopes;
mod value;

pub(crate) use analysis::*;
use class_index::*;
use collector_exprs::*;
pub(crate) use dataclasses_enums::*;
pub(crate) use evaluator::*;
pub(crate) use exports::*;
pub(crate) use folding::*;
pub(crate) use foreign::*;
use groups::*;
use methods::*;
pub(crate) use resolution::*;
pub(crate) use resolver::*;
use scopes::*;
pub(crate) use value::*;

const MAX_ELEMENTS: usize = 10_000;

const MAX_STEPS: u32 = 50_000;

const MAX_DEPTH: u32 = 64;

const MAX_STRING: usize = 100_000;

#[cfg(test)]
mod evaluator_tests;

#[cfg(test)]
mod analysis_tests;
