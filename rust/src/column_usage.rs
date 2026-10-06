//! Downstream column-usage observation for a DataFrame origin the checker recognized
//! but couldn't resolve a schema for (an `UntypedSite`).
//!
//! Records which columns the rest of a scope reads off such a frame, as evidence for
//! suggesting a column selection or schema. It makes no claim about correctness: there is
//! no schema here to validate against.
//!
//! A suggestion edits real code, so the scan errs toward recording an `Unknown` other-use:
//! anything it can't attribute to a column, or that lets the frame escape, is one.
//!
//! Scope limits:
//! - A nested function, class, lambda or comprehension body is its own scope and is not
//!   walked. `first_nested_scope_mention` reports whether any of them mention the frame,
//!   since a closure can read columns this scan never sees.
//! - A `.method()`/`.attribute` use is tagged with its [`Consumption`], but which names
//!   read every column or none is decided by the table in `column_consuming_methods`.

use crate::column_consuming_methods::classify_name;
use crate::contract::expr_forwards_tainted;
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_text_size::{Ranged, TextSize};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// One column subscripted directly off a tracked origin, or a variable derived from
/// it via straight reassignment/forwarding (same taint-growth rule
/// `contract::expr_forwards_tainted` uses for a function's own parameter contract).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColumnAccess {
    pub(crate) column: String,
    /// Reached only under a branch/loop/`with`/`try`, not on every execution path --
    /// still a real access, but the checker cannot promise it always happens.
    pub(crate) conditional: bool,
    /// 1-indexed source line. Not serialized: in a notebook it would be a line of the
    /// concatenated source. Only suggestions use it.
    #[serde(skip)]
    pub(crate) line: usize,
    /// The helper function this read happens inside, when the frame was passed to one.
    #[serde(skip)]
    pub(crate) via: Option<String>,
}

/// What a function does with one of its parameters, when every use of it is a read of a
/// named column. A parameter with any other use has no summary at all: the caller must
/// then treat passing a frame to it as an unknown use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ParamUsage {
    /// Index among the positional parameters; `usize::MAX` for a keyword-only one.
    pub(crate) position: usize,
    pub(crate) name: String,
    /// Each column read, and whether every such read is conditional.
    pub(crate) accesses: Vec<(String, bool)>,
}

/// Looks up what a called function does with its parameters.
pub(crate) trait HelperResolver {
    /// The summaries of the function `callee` names (one per parameter that has one), or
    /// `None` when the name can't be tied to a single known function.
    fn usage_of(&self, callee: &ast::ExprName) -> Option<Vec<ParamUsage>>;

    /// Whether `callee` is certainly the builtin of that name, not something this file
    /// defines, imports or rebinds.
    fn is_unshadowed_builtin(&self, callee: &ast::ExprName) -> bool;
}

/// A resolver that knows no function: every call is an unknown use.
#[cfg(test)]
pub(crate) struct NoHelpers;

#[cfg(test)]
impl HelperResolver for NoHelpers {
    fn usage_of(&self, _callee: &ast::ExprName) -> Option<Vec<ParamUsage>> {
        None
    }

    // Tests have no file to check against, so builtins are taken as unshadowed.
    fn is_unshadowed_builtin(&self, _callee: &ast::ExprName) -> bool {
        true
    }
}

/// Uses of the frame that narrowing the columns can't affect.
pub(crate) const HARMLESS_USES: &[&str] = &["`.index`", "`.empty`", "passed to `len()`"];

/// Prefix of the description of a use that only displays the frame. Narrowing changes what
/// is shown, never whether the program runs, so it asks for review rather than blocking.
pub(crate) const OUTPUT_USE_PREFIX: &str = "output via ";

/// Whether a use stops the frame's columns from being narrowed.
pub(crate) fn blocks_narrowing(other: &OtherUse) -> bool {
    !HARMLESS_USES.contains(&other.description.as_str())
        && !other.description.starts_with(OUTPUT_USE_PREFIX)
}

/// Whether a `.method()`/`.attribute` use is known to read every column's actual
/// values, known to read none (pure shape/metadata), or isn't classified -- see
/// `column_consuming_methods` for the table this is computed from. A use with no
/// single name (passed to a call, returned, iterated over, an unrecognized
/// subscript) is always `Unknown`: nothing here can say what it does with the
/// frame, so it's treated the same as an unrecognized name -- conservatively, as
/// still needing the columns nothing else accounted for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Consumption {
    All,
    None,
    Unknown,
}

/// A way the tracked origin (or a derived variable) was used besides a plain
/// `x["col"]`/`x[["a", "b"]]` subscript -- a method/attribute name, or a short label
/// for a shape with no single name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OtherUse {
    pub(crate) description: String,
    pub(crate) conditional: bool,
    pub(crate) consumes: Consumption,
    /// See `ColumnAccess::line`.
    #[serde(skip)]
    pub(crate) line: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ColumnUsage {
    pub(crate) accesses: Vec<ColumnAccess>,
    pub(crate) other_uses: Vec<OtherUse>,
    /// Every name that referred to the origin at any point during the scan (the root
    /// plus each alias it was assigned to). Feeds `first_nested_scope_mention`.
    pub(crate) ever_tainted: HashSet<String>,
    /// A column was assigned into the frame (`df["new"] = ...`) somewhere in the scan.
    pub(crate) wrote_columns: bool,
}

/// Scans `body` -- the statements that can run AFTER an origin, in its own scope -- for downstream
/// usage of `root`. `line_of` maps a source offset to its 1-indexed line.
#[cfg(test)]
pub(crate) fn scan_usage<'s>(
    root: &str,
    body: impl IntoIterator<Item = &'s Stmt>,
    line_of: &dyn Fn(TextSize) -> usize,
) -> ColumnUsage {
    scan_usage_with(root, body, line_of, &NoHelpers)
}

/// [`scan_usage`], following the frame into functions `helpers` can summarize.
pub(crate) fn scan_usage_with<'s>(
    root: &str,
    body: impl IntoIterator<Item = &'s Stmt>,
    line_of: &dyn Fn(TextSize) -> usize,
    helpers: &dyn HelperResolver,
) -> ColumnUsage {
    let mut scan = UsageScan {
        helpers,
        tainted: HashSet::from([root.to_string()]),
        depth: 0,
        written: HashSet::new(),
        usage: ColumnUsage {
            ever_tainted: HashSet::from([root.to_string()]),
            ..ColumnUsage::default()
        },
        suppressed_self_reference: None,
        line_of,
    };
    for stmt in body {
        scan.visit_stmt(stmt);
    }
    scan.usage
}

/// The offset of the first mention of any of `names` inside a nested function, class,
/// lambda or comprehension anywhere in `body` -- the scan above deliberately does not
/// walk those scopes, but a closure can read columns it never sees. Pass the WHOLE
/// scope body (not just the statements after the origin): a function defined above
/// the origin can still read the variable once called.
///
/// A comprehension's outermost iterable is evaluated in the enclosing scope and is
/// not a nested mention (the scan already records it); the same goes for decorators,
/// parameter defaults and class bases.
pub(crate) fn first_nested_scope_mention(
    body: &[Stmt],
    names: &HashSet<String>,
) -> Option<TextSize> {
    let mut finder = NestedMentionFinder {
        names,
        depth: 0,
        first: None,
    };
    for stmt in body {
        finder.visit_stmt(stmt);
    }
    finder.first
}

/// Whether `body` (not descending into nested functions/classes) declares `name`
/// `global` or `nonlocal` -- its assignments then bind a variable other code can
/// read, so the scope no longer contains every use of it.
pub(crate) fn declares_global_or_nonlocal(body: &[Stmt], name: &str) -> bool {
    let mut finder = ScopeDeclFinder { name, found: false };
    for stmt in body {
        finder.visit_stmt(stmt);
    }
    finder.found
}

struct NestedMentionFinder<'n> {
    names: &'n HashSet<String>,
    depth: u32,
    first: Option<TextSize>,
}

impl NestedMentionFinder<'_> {
    fn visit_parameter_defaults(&mut self, parameters: &ast::Parameters) {
        for param in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            if let Some(default) = &param.default {
                self.visit_expr(default);
            }
        }
    }

    fn visit_comprehension<'a>(
        &mut self,
        generators: &'a [ast::Comprehension],
        elements: &[&'a Expr],
    ) {
        if let Some(first) = generators.first() {
            self.visit_expr(&first.iter);
        }
        self.depth += 1;
        for (i, generator) in generators.iter().enumerate() {
            self.visit_expr(&generator.target);
            if i > 0 {
                self.visit_expr(&generator.iter);
            }
            for condition in &generator.ifs {
                self.visit_expr(condition);
            }
        }
        for element in elements {
            self.visit_expr(element);
        }
        self.depth -= 1;
    }
}

impl<'a> Visitor<'a> for NestedMentionFinder<'_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(f) => {
                for decorator in &f.decorator_list {
                    self.visit_expr(&decorator.expression);
                }
                self.visit_parameter_defaults(&f.parameters);
                self.depth += 1;
                for s in &f.body {
                    self.visit_stmt(s);
                }
                self.depth -= 1;
            }
            Stmt::ClassDef(c) => {
                for decorator in &c.decorator_list {
                    self.visit_expr(&decorator.expression);
                }
                if let Some(arguments) = &c.arguments {
                    for arg in &arguments.args {
                        self.visit_expr(arg);
                    }
                    for keyword in &arguments.keywords {
                        self.visit_expr(&keyword.value);
                    }
                }
                self.depth += 1;
                for s in &c.body {
                    self.visit_stmt(s);
                }
                self.depth -= 1;
            }
            _ => ast_visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) => {
                if self.depth > 0 && self.first.is_none() && self.names.contains(name.id.as_str()) {
                    self.first = Some(name.range().start());
                }
            }
            Expr::Lambda(lambda) => {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameter_defaults(parameters);
                }
                self.depth += 1;
                self.visit_expr(&lambda.body);
                self.depth -= 1;
            }
            Expr::ListComp(c) => self.visit_comprehension(&c.generators, &[&c.elt]),
            Expr::SetComp(c) => self.visit_comprehension(&c.generators, &[&c.elt]),
            Expr::Generator(c) => self.visit_comprehension(&c.generators, &[&c.elt]),
            Expr::DictComp(c) => {
                let mut elements: Vec<&Expr> = Vec::with_capacity(2);
                if let Some(key) = &c.key {
                    elements.push(key);
                }
                elements.push(&c.value);
                self.visit_comprehension(&c.generators, &elements);
            }
            _ => ast_visitor::walk_expr(self, expr),
        }
    }
}

struct ScopeDeclFinder<'n> {
    name: &'n str,
    found: bool,
}

impl<'a> Visitor<'a> for ScopeDeclFinder<'_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            Stmt::Global(g) => {
                if g.names.iter().any(|n| n.as_str() == self.name) {
                    self.found = true;
                }
            }
            Stmt::Nonlocal(n) => {
                if n.names.iter().any(|n| n.as_str() == self.name) {
                    self.found = true;
                }
            }
            _ => ast_visitor::walk_stmt(self, stmt),
        }
    }
}

// "passed to `clean()`" for `clean(df)`, "passed to `np.log()`" for `np.log(df)`, and the
// generic "passed to a call" when the callee isn't a plain name or `module.name`.
fn passed_to(func: &Expr) -> String {
    match func {
        Expr::Name(name) => format!("passed to `{}()`", name.id),
        Expr::Attribute(attr) => match &*attr.value {
            Expr::Name(base) => format!("passed to `{}.{}()`", base.id, attr.attr),
            _ => "passed to a call".to_string(),
        },
        _ => "passed to a call".to_string(),
    }
}

struct UsageScan<'a> {
    helpers: &'a dyn HelperResolver,
    tainted: HashSet<String>,
    // Nesting inside if/for/while/with/try/match -- marks an access/use `conditional`
    // rather than something every execution path is known to reach.
    depth: u32,
    // Columns assigned into the frame on every path so far (`df["new"] = ...`): a later
    // read of one is not a read of the source, so it isn't recorded as an access.
    written: HashSet<String>,
    usage: ColumnUsage,
    // While walking a tainted-receiver subscript's slice (`df[<slice>]`), the
    // receiver's own bare name is already summarized by the observation just
    // recorded for the whole subscript (specific columns, or "this subscript") --
    // set to that receiver's name for the duration of the slice walk so a bare
    // self-reference found inside it (`df[df > 0]`, `df[df.values]`) isn't ALSO
    // recorded as a second, separate use of the same logical operation. A name
    // found via something more specific than a bare reference (a nested
    // `df["col"]`) is unaffected -- it never reaches the suppressed checks below.
    suppressed_self_reference: Option<String>,
    line_of: &'a dyn Fn(TextSize) -> usize,
}

impl UsageScan<'_> {
    fn is_tainted(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(n) if self.tainted.contains(n.id.as_str()))
    }

    // `visit_expr` records a bare tainted `Name` reached generically (no more
    // specific handling applied) as an "Unknown" other-use -- see its own `Expr::Name`
    // arm. A pure alias (`out = df`, a walrus `y := df`) is NOT such a case: it's
    // already fully captured by `rebind_name`, so callers that are about to rebind
    // from this same expression call this instead of `visit_expr` directly, to avoid
    // double-reporting a plain rename as if it were an unrecognized use.
    fn visit_unless_tainted(&mut self, expr: &Expr) {
        if !self.is_tainted(expr) {
            self.visit_expr(expr);
        }
    }

    // Like `is_tainted`, but returns the name itself -- callers that need to set
    // `suppressed_self_reference` to it (Subscript) or check it against an already-
    // suppressed name (Attribute, Call) use this instead.
    fn tainted_name<'e>(&self, expr: &'e Expr) -> Option<&'e str> {
        match expr {
            Expr::Name(n) if self.tainted.contains(n.id.as_str()) => Some(n.id.as_str()),
            _ => None,
        }
    }

    fn is_suppressed(&self, name: &str) -> bool {
        self.suppressed_self_reference.as_deref() == Some(name)
    }

    // The frame is passed as an argument. If the callee's use of that parameter is known
    // it counts as those column reads, made here; otherwise it is an unknown use.
    fn passed_argument(
        &mut self,
        at: TextSize,
        passed: &str,
        callee: Option<&ast::ExprName>,
        summary: Option<&ParamUsage>,
    ) {
        let (Some(summary), Some(callee)) = (summary, callee) else {
            self.record_other(at, passed.to_string(), Consumption::Unknown);
            return;
        };
        let line = (self.line_of)(at);
        for (column, conditional) in &summary.accesses {
            // A column this scope created first isn't a source column.
            if self.written.contains(column) {
                continue;
            }
            self.usage.accesses.push(ColumnAccess {
                column: column.clone(),
                conditional: self.depth > 0 || *conditional,
                line,
                via: Some(callee.id.to_string()),
            });
        }
    }

    // Whether every tainted argument of `call` goes to a parameter whose use is known, so
    // the call's result is a new value rather than the tracked frame under another name
    // (a summarized function that handed its parameter back would not have a summary).
    fn call_is_fully_summarized(&self, call: &ast::ExprCall) -> bool {
        let Expr::Name(callee) = &*call.func else {
            return false;
        };
        let Some(summaries) = self.helpers.usage_of(callee) else {
            return false;
        };
        let positional_ok = call
            .arguments
            .args
            .iter()
            .enumerate()
            .filter(|(_, arg)| self.is_tainted(arg))
            .all(|(i, _)| summaries.iter().any(|p| p.position == i));
        let keywords_ok = call
            .arguments
            .keywords
            .iter()
            .filter(|kw| self.is_tainted(&kw.value))
            .all(|kw| {
                kw.arg
                    .as_ref()
                    .is_some_and(|name| summaries.iter().any(|p| p.name == name.as_str()))
            });
        positional_ok && keywords_ok
    }

    // Whether `value` hands the tracked frame on: a plain alias or a call that returns it,
    // but not a call whose use of the frame is fully known.
    fn forwards(&self, value: &Expr) -> bool {
        expr_forwards_tainted(&self.tainted, value)
            && !matches!(value, Expr::Call(call) if self.call_is_fully_summarized(call))
    }

    fn taint(&mut self, name: &str) {
        self.tainted.insert(name.to_string());
        self.usage.ever_tainted.insert(name.to_string());
    }

    // A name being assigned either joins the tainted set (its value forwards an
    // already-tainted one) or, if it was PREVIOUSLY tainted, leaves it -- a name
    // reassigned to something unrelated no longer refers to the tracked origin, the
    // same "the old leg ends here" rule `Linter::end_stale_legs` applies elsewhere.
    // Without this, a name reassigned away from the origin would keep contributing
    // observations that aren't actually about it.
    //
    // Only true for a reassignment that always runs: under a branch it may or may
    // not happen, so a later read could still be of the original frame. Keep the
    // taint and record an unclassified use instead of dropping reads on the floor.
    fn rebind_name(&mut self, at: TextSize, name: &str, forwards: bool) {
        if forwards {
            self.taint(name);
        } else if self.tainted.contains(name) {
            if self.depth > 0 {
                self.record_other(
                    at,
                    format!("`{name}` conditionally reassigned"),
                    Consumption::Unknown,
                );
            } else {
                self.tainted.remove(name);
            }
        }
    }

    fn record_column(&mut self, at: TextSize, column: String) {
        if self.written.contains(&column) {
            return;
        }
        let line = (self.line_of)(at);
        self.usage.accesses.push(ColumnAccess {
            column,
            conditional: self.depth > 0,
            line,
            via: None,
        });
    }

    fn record_other(
        &mut self,
        at: TextSize,
        description: impl Into<String>,
        consumes: Consumption,
    ) {
        let line = (self.line_of)(at);
        self.usage.other_uses.push(OtherUse {
            description: description.into(),
            conditional: self.depth > 0,
            consumes,
            line,
        });
    }

    fn visit_body(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.visit_stmt(stmt);
        }
    }

    fn visit_conditional_body(&mut self, body: &[Stmt]) {
        self.depth += 1;
        self.visit_body(body);
        self.depth -= 1;
    }

    // See the `Expr::ListComp` etc. arms in `visit_expr` for why only the first
    // generator's iterable is visited here.
    fn visit_outer_comprehension_iter(&mut self, generators: &[ast::Comprehension]) {
        let Some(first) = generators.first() else {
            return;
        };
        if self.is_tainted(&first.iter) {
            self.record_other(
                first.iter.range().start(),
                "iterated over directly",
                Consumption::Unknown,
            );
        } else {
            self.visit_expr(&first.iter);
        }
    }

    fn visit_interpolated<'e>(
        &mut self,
        elements: impl Iterator<Item = &'e ast::InterpolatedStringElement>,
    ) {
        for element in elements {
            if let ast::InterpolatedStringElement::Interpolation(interp) = element {
                self.visit_expr(&interp.expression);
                if let Some(spec) = &interp.format_spec {
                    self.visit_interpolated(spec.elements.iter());
                }
            }
        }
    }

    // Parameter defaults, decorators and class bases are evaluated in the ENCLOSING
    // scope when the `def`/`class` statement runs, unlike the body.
    fn visit_parameter_defaults(&mut self, parameters: &ast::Parameters) {
        for param in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            if let Some(default) = &param.default {
                self.visit_expr(default);
            }
        }
    }

    // The target of an assignment, `for`, `with` or `except`. `forwards` is `Some` for
    // an assignment (whether its value is, or forwards, the tracked frame) and `None`
    // for a loop/`with`/`except` target, which rebinds a name without our being able
    // to say what to.
    fn visit_store_target(&mut self, target: &Expr, forwards: Option<bool>) {
        let at = target.range().start();
        match target {
            Expr::Name(name) => match forwards {
                Some(forwards) => self.rebind_name(at, name.id.as_str(), forwards),
                None => {
                    if self.tainted.contains(name.id.as_str()) {
                        self.record_other(
                            at,
                            format!("`{}` rebound by a loop, `with` or `except` target", name.id),
                            Consumption::Unknown,
                        );
                    }
                }
            },
            Expr::Tuple(t) => {
                for el in &t.elts {
                    self.visit_store_target(el, forwards.map(|_| false));
                }
            }
            Expr::List(l) => {
                for el in &l.elts {
                    self.visit_store_target(el, forwards.map(|_| false));
                }
            }
            Expr::Starred(s) => self.visit_store_target(&s.value, forwards.map(|_| false)),
            Expr::Subscript(sub) => {
                if let Some(receiver) = self.tainted_name(&sub.value) {
                    // Writing into the frame: `df["new"] = ...` is not a read, so a
                    // literal column here is NOT recorded as an access (doing so would
                    // make a column that may not exist yet look required). Anything
                    // else -- a mask, a computed key -- can touch columns we can't
                    // enumerate.
                    match crate::ast_extract::extract_string_list_or_single(&sub.slice) {
                        None => self.record_other(
                            at,
                            "assignment through this subscript",
                            Consumption::Unknown,
                        ),
                        Some(columns) => {
                            self.usage.wrote_columns = true;
                            // Only an unconditional write guarantees the column exists
                            // afterwards.
                            if self.depth == 0 {
                                self.written.extend(columns);
                            }
                        }
                    }
                    let previous = self.suppressed_self_reference.replace(receiver.to_string());
                    self.visit_expr(&sub.slice);
                    self.suppressed_self_reference = previous;
                } else {
                    self.visit_expr(&sub.value);
                    self.visit_expr(&sub.slice);
                }
            }
            Expr::Attribute(attr) => {
                if self.tainted_name(&attr.value).is_some() {
                    self.record_other(
                        at,
                        format!("`.{}` assigned", attr.attr.as_str()),
                        Consumption::Unknown,
                    );
                } else {
                    self.visit_expr(&attr.value);
                }
            }
            _ => {}
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        let at = stmt.range().start();
        match stmt {
            // A nested scope has its own bindings; the taint set built up so far
            // doesn't carry into it (matches `frame_ops::PassthroughScan`'s own
            // treatment of a nested def/class). What the statement itself evaluates in
            // THIS scope -- decorators, defaults, class bases -- is still visited, and
            // `first_nested_scope_mention` covers the body.
            Stmt::FunctionDef(f) => {
                for decorator in &f.decorator_list {
                    self.visit_expr(&decorator.expression);
                }
                self.visit_parameter_defaults(&f.parameters);
            }
            Stmt::ClassDef(c) => {
                for decorator in &c.decorator_list {
                    self.visit_expr(&decorator.expression);
                }
                if let Some(arguments) = &c.arguments {
                    for arg in &arguments.args {
                        self.visit_expr(arg);
                    }
                    for keyword in &arguments.keywords {
                        self.visit_expr(&keyword.value);
                    }
                }
            }
            Stmt::Return(ret) => match &ret.value {
                Some(value) if self.is_tainted(value) => {
                    self.record_other(at, "returned", Consumption::Unknown)
                }
                Some(value) => self.visit_expr(value),
                None => {}
            },
            Stmt::Expr(expr_stmt) => self.visit_expr(&expr_stmt.value),
            Stmt::Assign(assign) => {
                // A bare `out = df` is pure aliasing, already fully captured by
                // `rebind_name` below -- must NOT also fall through to `visit_expr`'s
                // generic `Expr::Name` fallback, or a plain rename would wrongly
                // register as an "unrecognized use" alongside becoming tainted. But
                // stored into an attribute or container (`self.df = df`,
                // `cache["k"] = df`) the frame escapes this scan's view entirely.
                let escapes = self.is_tainted(&assign.value)
                    && assign.targets.iter().any(|t| !matches!(t, Expr::Name(_)));
                if escapes {
                    self.record_other(
                        at,
                        "stored in an attribute or container",
                        Consumption::Unknown,
                    );
                } else {
                    self.visit_unless_tainted(&assign.value);
                }
                let forwards = self.forwards(&assign.value);
                for target in &assign.targets {
                    self.visit_store_target(target, Some(forwards));
                }
            }
            Stmt::AnnAssign(ann) => {
                if let Some(value) = &ann.value {
                    let escapes = self.is_tainted(value) && !matches!(&*ann.target, Expr::Name(_));
                    if escapes {
                        self.record_other(
                            at,
                            "stored in an attribute or container",
                            Consumption::Unknown,
                        );
                    } else {
                        self.visit_unless_tainted(value);
                    }
                    let forwards = self.forwards(value);
                    self.visit_store_target(&ann.target, Some(forwards));
                }
            }
            // `df["b"] += 1` READS `b` before writing it, so the target is visited as
            // a load -- unlike a plain assignment's.
            Stmt::AugAssign(aug) => {
                self.visit_expr(&aug.value);
                self.visit_expr(&aug.target);
            }
            Stmt::Delete(del) => {
                for target in &del.targets {
                    self.visit_expr(target);
                }
            }
            Stmt::If(if_stmt) => {
                self.visit_expr(&if_stmt.test);
                self.visit_conditional_body(&if_stmt.body);
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.visit_expr(test);
                    }
                    self.visit_conditional_body(&clause.body);
                }
            }
            Stmt::For(for_stmt) => {
                if self.is_tainted(&for_stmt.iter) {
                    self.record_other(
                        for_stmt.iter.range().start(),
                        "iterated over directly",
                        Consumption::Unknown,
                    );
                } else {
                    self.visit_expr(&for_stmt.iter);
                }
                self.visit_store_target(&for_stmt.target, None);
                self.visit_conditional_body(&for_stmt.body);
                self.visit_conditional_body(&for_stmt.orelse);
            }
            Stmt::While(while_stmt) => {
                self.visit_expr(&while_stmt.test);
                self.visit_conditional_body(&while_stmt.body);
                self.visit_conditional_body(&while_stmt.orelse);
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr);
                    if let Some(vars) = &item.optional_vars {
                        self.visit_store_target(vars, None);
                    }
                }
                self.visit_conditional_body(&with_stmt.body);
            }
            Stmt::Try(try_stmt) => {
                self.visit_conditional_body(&try_stmt.body);
                for handler in &try_stmt.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    if let Some(type_) = &h.type_ {
                        self.visit_expr(type_);
                    }
                    if let Some(name) = &h.name {
                        if self.tainted.contains(name.as_str()) {
                            self.record_other(
                                h.range().start(),
                                format!("`{name}` rebound by a loop, `with` or `except` target"),
                                Consumption::Unknown,
                            );
                        }
                    }
                    self.visit_conditional_body(&h.body);
                }
                self.visit_conditional_body(&try_stmt.orelse);
                self.visit_conditional_body(&try_stmt.finalbody);
            }
            Stmt::Assert(assert_stmt) => {
                self.visit_expr(&assert_stmt.test);
                if let Some(msg) = &assert_stmt.msg {
                    self.visit_expr(msg);
                }
            }
            Stmt::Raise(raise_stmt) => {
                if let Some(exc) = &raise_stmt.exc {
                    self.visit_expr(exc);
                }
                if let Some(cause) = &raise_stmt.cause {
                    self.visit_expr(cause);
                }
            }
            Stmt::Match(match_stmt) => {
                self.visit_expr(&match_stmt.subject);
                for case in &match_stmt.cases {
                    // A case only runs if its pattern (and guard) matched -- every
                    // case is conditional, the same as an `if`/`elif` branch.
                    self.depth += 1;
                    if let Some(guard) = &case.guard {
                        self.visit_expr(guard);
                    }
                    self.visit_body(&case.body);
                    self.depth -= 1;
                }
            }
            _ => {}
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        let at = expr.range().start();
        match expr {
            Expr::Subscript(sub) => {
                if let Some(receiver) = self.tainted_name(&sub.value) {
                    match crate::ast_extract::extract_string_list_or_single(&sub.slice) {
                        Some(cols) => {
                            for col in cols {
                                self.record_column(at, col);
                            }
                        }
                        None => self.record_other(at, "this subscript", Consumption::Unknown),
                    }
                    // The whole subscript is already summarized by the observation
                    // just recorded above (specific columns, or "this subscript" for
                    // an unrecognized shape) -- suppress a bare self-reference found
                    // while walking the slice (`df[df > 0]`, `df[df.values]`), or one
                    // subscript operation double-counts as two separate uses. A
                    // MORE SPECIFIC nested reference (`df[df["age"] > 30]`'s inner
                    // `df["age"]`) is unaffected -- it's a real, distinct fact (which
                    // column), not a restatement of this one.
                    let previous = self.suppressed_self_reference.replace(receiver.to_string());
                    self.visit_expr(&sub.slice);
                    self.suppressed_self_reference = previous;
                } else {
                    self.visit_expr(&sub.value);
                    self.visit_expr(&sub.slice);
                }
            }
            Expr::Attribute(attr) => {
                if let Some(receiver) = self.tainted_name(&attr.value) {
                    if !self.is_suppressed(receiver) {
                        let name = attr.attr.as_str();
                        self.record_other(at, format!("`.{name}`"), classify_name(name));
                    }
                } else {
                    self.visit_expr(&attr.value);
                }
            }
            Expr::Call(call) => {
                // These can read or rebind ANY variable by name, so nothing about the
                // frame can be ruled out once one is called in this scope.
                if let Expr::Name(f) = &*call.func {
                    if matches!(
                        f.id.as_str(),
                        "eval" | "exec" | "locals" | "globals" | "vars"
                    ) {
                        self.record_other(
                            at,
                            format!("`{}()` can reach any variable", f.id),
                            Consumption::Unknown,
                        );
                    }
                }
                match &*call.func {
                    // A method call ON the tainted var: `var.method(...)` -- one
                    // "other use" for the whole call, not also the generic
                    // attribute-access case above.
                    Expr::Attribute(attr) => match self.tainted_name(&attr.value) {
                        Some(receiver) if !self.is_suppressed(receiver) => {
                            let name = attr.attr.as_str();
                            self.record_other(at, format!("`.{name}()`"), classify_name(name));
                        }
                        Some(_) => {}
                        None => self.visit_expr(&call.func),
                    },
                    _ => self.visit_expr(&call.func),
                }
                let callee = match &*call.func {
                    Expr::Name(f) => Some(f),
                    _ => None,
                };
                let builtin = callee.filter(|f| self.helpers.is_unshadowed_builtin(f));
                let passed = match (builtin, &*call.func) {
                    (Some(f), _) if matches!(f.id.as_str(), "print" | "display") => {
                        format!("{OUTPUT_USE_PREFIX}`{}()`", f.id)
                    }
                    // Something else named `len` could do anything with the frame.
                    (None, Expr::Name(f)) if f.id.as_str() == "len" => {
                        "passed to a function named `len()` that isn't the builtin".to_string()
                    }
                    _ => passed_to(&call.func),
                };
                let summaries = callee.and_then(|f| self.helpers.usage_of(f));
                for (position, arg) in call.arguments.args.iter().enumerate() {
                    if self.is_tainted(arg) {
                        let summary = summaries
                            .as_ref()
                            .and_then(|all| all.iter().find(|p| p.position == position));
                        self.passed_argument(at, &passed, callee, summary);
                    } else {
                        self.visit_expr(arg);
                    }
                }
                for kw in &call.arguments.keywords {
                    if self.is_tainted(&kw.value) {
                        let summary = kw.arg.as_ref().and_then(|name| {
                            summaries
                                .as_ref()
                                .and_then(|all| all.iter().find(|p| p.name == name.as_str()))
                        });
                        self.passed_argument(at, &passed, callee, summary);
                    } else {
                        self.visit_expr(&kw.value);
                    }
                }
            }
            // A bare reference reached generically -- nothing more specific above
            // (Subscript/Attribute/Call-as-receiver/Call-argument) already recorded
            // it. Still a real use the checker can't otherwise characterize (e.g.
            // `pd.concat([df, other])`, `df + 1`, `total += df`) -- record it rather
            // than silently drop it, consistent with this module's own stated
            // philosophy of recording every observation it can, even unclassified.
            Expr::Name(name)
                if self.tainted.contains(name.id.as_str())
                    && !self.is_suppressed(name.id.as_str()) =>
            {
                self.record_other(at, "referenced", Consumption::Unknown);
            }
            Expr::Name(_) => {}
            // Same treatment as `FunctionDef`/`ClassDef` in `visit_stmt`: a lambda
            // body is its own scope, not walked (its parameter defaults are not).
            Expr::Lambda(lambda) => {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameter_defaults(parameters);
                }
            }
            Expr::Named(named) => {
                // A walrus target (`y := df`) binds a real name in the ENCLOSING
                // scope -- unlike `Lambda`, this isn't a nested scope, so it needs
                // the same alias-vs-use handling `Stmt::Assign` gets, not a skip.
                self.visit_unless_tainted(&named.value);
                if let Expr::Name(t) = &*named.target {
                    let forwards = self.forwards(&named.value);
                    self.rebind_name(t.range().start(), t.id.as_str(), forwards);
                }
            }
            Expr::Dict(dict) => {
                for item in &dict.items {
                    if let Some(key) = &item.key {
                        self.visit_expr(key);
                    }
                    self.visit_expr(&item.value);
                }
            }
            Expr::Set(set) => {
                for el in &set.elts {
                    self.visit_expr(el);
                }
            }
            // A comprehension/generator expression is its own scope, like `Lambda`
            // -- EXCEPT for one CPython-documented exception: the iterable of the
            // OUTERMOST `for` clause is evaluated in the enclosing scope, before the
            // comprehension's own scope even exists. That one sub-expression is a
            // real access happening HERE (e.g. `sum(x for x in df["a"])` must still
            // record `"a"`); the element/key/value, `if` filters, and any nested
            // `for` clauses are not walked, matching `Lambda`.
            Expr::ListComp(comp) => self.visit_outer_comprehension_iter(&comp.generators),
            Expr::SetComp(comp) => self.visit_outer_comprehension_iter(&comp.generators),
            Expr::DictComp(comp) => self.visit_outer_comprehension_iter(&comp.generators),
            Expr::Generator(comp) => self.visit_outer_comprehension_iter(&comp.generators),
            Expr::Yield(y) => {
                if let Some(value) = &y.value {
                    self.visit_expr(value);
                }
            }
            Expr::YieldFrom(yf) => self.visit_expr(&yf.value),
            Expr::Slice(slice) => {
                if let Some(lower) = &slice.lower {
                    self.visit_expr(lower);
                }
                if let Some(upper) = &slice.upper {
                    self.visit_expr(upper);
                }
                if let Some(step) = &slice.step {
                    self.visit_expr(step);
                }
            }
            Expr::FString(fstring) => self.visit_interpolated(fstring.value.elements()),
            Expr::TString(tstring) => self.visit_interpolated(tstring.value.elements()),
            Expr::BinOp(binop) => {
                self.visit_expr(&binop.left);
                self.visit_expr(&binop.right);
            }
            Expr::BoolOp(boolop) => {
                for v in &boolop.values {
                    self.visit_expr(v);
                }
            }
            Expr::UnaryOp(unary) => self.visit_expr(&unary.operand),
            Expr::Compare(compare) => {
                self.visit_expr(&compare.left);
                for comp in compare.comparators.iter() {
                    self.visit_expr(comp);
                }
            }
            Expr::List(list) => {
                for el in &list.elts {
                    self.visit_expr(el);
                }
            }
            Expr::Tuple(tuple) => {
                for el in &tuple.elts {
                    self.visit_expr(el);
                }
            }
            Expr::Starred(starred) => self.visit_expr(&starred.value),
            Expr::If(if_expr) => {
                self.visit_expr(&if_expr.test);
                self.visit_expr(&if_expr.body);
                self.visit_expr(&if_expr.orelse);
            }
            Expr::Await(awaited) => self.visit_expr(&awaited.value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;

    fn usage(source: &str) -> ColumnUsage {
        let module = parse_module(source).unwrap().into_syntax();
        scan_usage("df", &module.body, &|_| 0)
    }

    fn cols(usage: &ColumnUsage) -> Vec<(&str, bool)> {
        usage
            .accesses
            .iter()
            .map(|a| (a.column.as_str(), a.conditional))
            .collect()
    }

    fn others(usage: &ColumnUsage) -> Vec<(&str, bool, Consumption)> {
        usage
            .other_uses
            .iter()
            .map(|u| (u.description.as_str(), u.conditional, u.consumes))
            .collect()
    }

    #[test]
    fn test_should_record_an_unconditional_column_access() {
        let u = usage("print(df[\"a\"])\nprint(df[\"b\"])\n");
        assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_record_a_list_subscript_as_multiple_accesses() {
        let u = usage("print(df[[\"a\", \"b\"]])\n");
        assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
    }

    #[test]
    fn test_should_mark_an_access_inside_a_branch_as_conditional() {
        let u = usage("if flag:\n    print(df[\"a\"])\nprint(df[\"b\"])\n");
        assert_eq!(cols(&u), vec![("a", true), ("b", false)]);
    }

    #[test]
    fn test_should_mark_accesses_inside_for_while_with_try_as_conditional() {
        let u = usage(
            "for x in xs:\n    print(df[\"a\"])\nwhile flag:\n    print(df[\"b\"])\nwith open(p) as f:\n    print(df[\"c\"])\ntry:\n    print(df[\"d\"])\nexcept Exception:\n    print(df[\"e\"])\n",
        );
        assert_eq!(
            cols(&u),
            vec![
                ("a", true),
                ("b", true),
                ("c", true),
                ("d", true),
                ("e", true)
            ]
        );
    }

    #[test]
    fn test_should_follow_a_variable_derived_from_the_root_via_reassignment() {
        let u = usage("out = df\nprint(out[\"a\"])\n");
        assert_eq!(cols(&u), vec![("a", false)]);
        // A pure alias must not ALSO register as a generic "referenced" use of the
        // RHS `df` -- it's fully captured by becoming tainted, not an unrecognized
        // use (regression test: `visit_expr`'s bare-Name fallback must not fire here).
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_follow_a_variable_derived_via_a_call_forwarding_the_root() {
        let u = usage("out = clean(df)\nprint(out[\"a\"])\n");
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_not_follow_an_unrelated_variable() {
        let u = usage("other = something_else()\nprint(other[\"a\"])\n");
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_record_a_method_call_as_an_other_use_not_an_access() {
        let u = usage("df.to_dict()\n");
        assert_eq!(others(&u), vec![("`.to_dict()`", false, Consumption::All)]);
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_record_an_attribute_access_as_an_other_use() {
        let u = usage("print(df.columns)\n");
        assert_eq!(others(&u), vec![("`.columns`", false, Consumption::None)]);
    }

    #[test]
    fn test_should_record_being_passed_to_a_call_as_an_other_use() {
        let u = usage("helper(df)\n");
        assert_eq!(
            others(&u),
            vec![("passed to `helper()`", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_being_returned_as_an_other_use() {
        let u = usage("def f():\n    return df\n");
        // `return` inside a nested function isn't walked (own scope) -- construct a
        // bare top-level return instead via a for-loop body to keep it in scope.
        assert!(u.other_uses.is_empty());
        let u2 = usage("if flag:\n    return df\n");
        assert_eq!(others(&u2), vec![("returned", true, Consumption::Unknown)]);
    }

    #[test]
    fn test_should_record_a_direct_iteration_as_an_other_use() {
        // arrange / act: the iterable itself is evaluated once, unconditionally, the
        // moment the `for` statement runs -- the LOOP BODY is what's conditional
        // (depth only increments once entering it, checked separately below).
        let u = usage("for col in df:\n    print(col)\n");

        // assert
        assert_eq!(
            others(&u),
            vec![("iterated over directly", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_mark_the_for_loop_body_conditional_but_not_its_header() {
        // arrange / act
        let u = usage("for x in xs:\n    print(df[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", true)]);
    }

    #[test]
    fn test_should_record_an_unrecognized_subscript_as_an_other_use() {
        let u = usage("mask = df[\"a\"] > 1\nprint(df[mask])\n");
        // `df["a"]` in the mask expression is a real, unconditional access; the
        // boolean-mask subscript on the next line is an unrecognized shape.
        assert_eq!(cols(&u), vec![("a", false)]);
        assert_eq!(
            others(&u),
            vec![("this subscript", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_classify_an_unrecognized_method_name_as_unknown() {
        let u = usage("df.some_custom_transform()\n");
        assert_eq!(
            others(&u),
            vec![("`.some_custom_transform()`", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_classify_a_non_consuming_attribute_by_name() {
        let u = usage("print(df.shape)\n");
        assert_eq!(others(&u), vec![("`.shape`", false, Consumption::None)]);
    }

    #[test]
    fn test_should_not_descend_into_a_nested_function_or_class() {
        let u = usage("def f():\n    print(df[\"a\"])\n\n\nclass C:\n    x = df[\"b\"]\n");
        assert!(u.accesses.is_empty());
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_stop_growing_taint_from_an_unrelated_reassignment() {
        // `df = other()` doesn't forward the root, so `df` is no longer tainted from
        // that point -- matches `expr_forwards_tainted`'s own rule.
        let u = usage("df = other()\nprint(df[\"a\"])\n");
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_untaint_a_name_reassigned_away_from_the_origin() {
        // arrange / act: `out` picks up the taint from `df`, then loses it once
        // reassigned to something unrelated -- the access after that must not be
        // recorded as if it were still about the original origin.
        let u = usage("out = df\nout = other()\nprint(out[\"a\"])\n");

        // assert
        assert!(u.accesses.is_empty());
    }

    #[test]
    fn test_should_keep_the_root_tainted_when_a_derived_alias_is_reassigned() {
        // arrange / act: only `out` loses taint here, not `df` itself.
        let u = usage("out = df\nout = other()\nprint(df[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_record_a_bare_reference_nested_in_a_list_literal() {
        // arrange / act: regression test -- `pd.concat([df, other])` used to
        // produce ZERO observations (not even Unknown), since `Expr::List` visits
        // its elements generically with no taint check, and there was no
        // `Expr::Name` fallback at all for a bare reference reached that way.
        let u = usage("pd.concat([df, other])\n");

        // assert
        assert_eq!(
            others(&u),
            vec![("referenced", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_a_bare_reference_in_a_binop() {
        let u = usage("total = df + 1\n");
        assert_eq!(
            others(&u),
            vec![("referenced", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_a_bare_reference_in_an_augmented_assignment() {
        let u = usage("total += df\n");
        assert_eq!(
            others(&u),
            vec![("referenced", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_not_record_anything_for_an_unrelated_name() {
        // arrange / act: the new `Expr::Name` fallback must only fire for the
        // TAINTED name, not any bare name reached the same way.
        let u = usage("pd.concat([other, another])\n");

        // assert
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_record_an_assert_expression_use() {
        let u = usage("assert df[\"a\"].notna().all()\n");
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_record_a_raise_expression_use() {
        let u = usage("raise ValueError(df)\n");
        assert_eq!(
            others(&u),
            vec![("passed to `ValueError()`", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_mark_a_match_case_body_as_conditional() {
        let u = usage("match x:\n    case 1:\n        print(df[\"a\"])\n");
        assert_eq!(cols(&u), vec![("a", true)]);
    }

    #[test]
    fn test_should_record_a_dict_literal_value_as_a_use() {
        let u = usage("summary = {\"total\": df}\n");
        assert_eq!(
            others(&u),
            vec![("referenced", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_a_set_literal_element_as_a_use() {
        let u = usage("s = {df}\n");
        assert_eq!(
            others(&u),
            vec![("referenced", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_an_fstring_interpolation_as_a_use() {
        let u = usage("msg = f\"{df}\"\n");
        assert_eq!(
            others(&u),
            vec![("referenced", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_a_slice_bound_as_a_use() {
        let u = usage("out = other[df:]\n");
        assert_eq!(
            others(&u),
            vec![("referenced", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_record_a_yield_value_as_a_use() {
        // arrange / act: `yield` inside a nested `def` isn't walked (own scope,
        // same as `return` -- see `test_should_record_being_returned_as_an_other_use`)
        // -- construct a bare top-level yield instead, the same way that test does.
        let u = usage("if flag:\n    yield df\n");

        // assert
        assert_eq!(others(&u), vec![("referenced", true, Consumption::Unknown)]);
    }

    #[test]
    fn test_should_not_descend_into_a_lambda_body() {
        // arrange / act: same "own scope" treatment as a nested `def`.
        let u = usage("f = lambda: df[\"a\"]\n");

        // assert
        assert!(u.accesses.is_empty());
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_record_the_outer_generator_iterable_as_a_real_access() {
        // arrange / act: CPython evaluates the outermost `for` clause's iterable in
        // the ENCLOSING scope, before the generator's own scope exists -- this is a
        // real, unconditional access happening here, not inside a skipped scope.
        let u = usage("total = sum(x for x in df[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_not_descend_into_a_comprehension_body() {
        // arrange / act: the element expression and any nested `for` clauses ARE the
        // comprehension's own scope -- unlike the outermost iterable, not walked.
        let u = usage("out = [df[\"a\"] for x in xs]\n");

        // assert
        assert!(u.accesses.is_empty());
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_track_a_walrus_assignment_target() {
        // arrange / act: `y := df` binds `y` in the ENCLOSING scope, unlike a
        // lambda/comprehension -- must be tracked the same as a plain `y = df`.
        let u = usage("if (y := df):\n    pass\nprint(y[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_not_double_report_a_walrus_pure_alias() {
        // arrange / act: regression test, the `Expr::Named` counterpart of
        // `test_should_follow_a_variable_derived_from_the_root_via_reassignment` --
        // a pure walrus alias must not ALSO register as a generic "referenced" use.
        let u = usage("y = (z := df)\n");

        // assert
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_not_double_report_an_annassign_pure_alias() {
        // arrange / act: the `Stmt::AnnAssign` counterpart of
        // `test_should_follow_a_variable_derived_from_the_root_via_reassignment` --
        // same guard, previously untested for this specific statement form.
        let u = usage("out: object = df\nprint(out[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", false)]);
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_not_double_report_a_self_referencing_boolean_mask() {
        // arrange / act: regression test -- `df[df > 0]` is a real, documented
        // whole-frame boolean-mask idiom. The tainted `Expr::Subscript` branch
        // already records ONE observation for the whole operation ("this
        // subscript"); the slice must not ALSO be walked generically afterward, or
        // the bare `df` inside the mask gets counted as a SECOND, separate use of
        // the same logical operation.
        let u = usage("print(df[df > 0])\n");

        // assert
        assert_eq!(
            others(&u),
            vec![("this subscript", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_not_double_report_a_self_referencing_attribute_mask() {
        // arrange / act: same root cause, via the `Expr::Attribute` arm instead --
        // `df[df.values]` must not record both "this subscript" and `.values`.
        let u = usage("print(df[df.values])\n");

        // assert
        assert_eq!(
            others(&u),
            vec![("this subscript", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_still_record_a_real_column_access_in_a_mask_built_from_one_column() {
        // arrange / act: the much more common "mask built from one column" idiom,
        // `df[df["age"] > 30]`, must NOT be affected by the self-mask fix above --
        // the inner occurrence is a proper Subscript (a real access), not a bare
        // Name, so it was never part of the double-count in the first place.
        let u = usage("print(df[df[\"age\"] > 30])\n");

        // assert
        assert_eq!(cols(&u), vec![("age", false)]);
        assert_eq!(
            others(&u),
            vec![("this subscript", false, Consumption::Unknown)]
        );
    }

    // --- soundness: what a suggestion that edits code depends on ---

    fn has_other(u: &ColumnUsage, description: &str) -> bool {
        u.other_uses.iter().any(|o| o.description == description)
    }

    #[test]
    fn test_should_not_record_a_column_write_as_an_access() {
        // arrange / act: `df["new"] = ...` creates/overwrites a column; recording it
        // as a read would make a column that may not exist in the source look required.
        let u = usage("df[\"new\"] = 1\n");

        // assert
        assert!(u.accesses.is_empty());
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_still_record_the_reads_on_the_right_of_a_column_write() {
        let u = usage("df[\"b\"] = df[\"a\"] * 2\n");
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_not_record_a_read_of_a_column_the_scope_wrote_first() {
        let u = usage("df[\"t\"] = df[\"a\"] * 2\nprint(df[\"t\"], df[\"b\"])\n");
        assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
        assert!(u.wrote_columns);
    }

    #[test]
    fn test_should_not_treat_a_conditional_write_as_creating_the_column() {
        let u = usage("if flag:\n    df[\"c\"] = 1\nprint(df[\"c\"])\n");
        assert_eq!(cols(&u), vec![("c", false)]);
        assert!(u.wrote_columns);
    }

    #[test]
    fn test_should_read_a_column_before_it_is_written_as_a_source_read() {
        let u = usage("print(df[\"x\"])\ndf[\"x\"] = 1\nprint(df[\"x\"])\n");
        assert_eq!(cols(&u), vec![("x", false)]);
    }

    #[test]
    fn test_should_count_a_list_assignment_as_writing_each_column() {
        let u = usage("df[[\"p\", \"q\"]] = f()\nprint(df[\"p\"], df[\"q\"])\n");
        assert!(cols(&u).is_empty());
    }

    #[test]
    fn test_should_record_an_augmented_assignment_on_a_column_as_a_read() {
        // `df["b"] += 1` reads `b` before it writes it.
        let u = usage("df[\"b\"] += 1\n");
        assert_eq!(cols(&u), vec![("b", false)]);
    }

    #[test]
    fn test_should_record_a_masked_assignment_as_unclassified() {
        let u = usage("df[df[\"a\"] > 1] = 0\n");
        assert!(has_other(&u, "assignment through this subscript"));
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_record_an_attribute_assignment_on_the_frame() {
        // `df.columns = [...]` renames every column.
        let u = usage("df.columns = [\"x\", \"y\"]\n");
        assert!(has_other(&u, "`.columns` assigned"));
    }

    #[test]
    fn test_should_record_the_frame_escaping_into_an_attribute() {
        let u = usage("self.df = df\n");
        assert_eq!(
            others(&u),
            vec![(
                "stored in an attribute or container",
                false,
                Consumption::Unknown
            )]
        );
    }

    #[test]
    fn test_should_record_the_frame_escaping_into_a_container() {
        let u = usage("cache[\"k\"] = df\n");
        assert!(has_other(&u, "stored in an attribute or container"));
    }

    #[test]
    fn test_should_visit_a_for_else_body() {
        let u = usage("for x in xs:\n    pass\nelse:\n    print(df[\"a\"])\n");
        assert_eq!(cols(&u), vec![("a", true)]);
    }

    #[test]
    fn test_should_visit_a_while_else_body() {
        let u = usage("while flag:\n    pass\nelse:\n    print(df[\"a\"])\n");
        assert_eq!(cols(&u), vec![("a", true)]);
    }

    #[test]
    fn test_should_keep_the_taint_when_a_conditional_reassignment_may_not_run() {
        // arrange / act: after the `if`, `df` may still be the ORIGINAL frame, so the
        // later read must still count -- and the reassignment must be flagged.
        let u = usage("if flag:\n    df = other()\nprint(df[\"a\"])\n");

        // assert
        assert_eq!(cols(&u), vec![("a", false)]);
        assert!(has_other(&u, "`df` conditionally reassigned"));
    }

    #[test]
    fn test_should_flag_a_loop_variable_that_rebinds_the_frame() {
        let u = usage("for df in frames:\n    pass\n");
        assert!(has_other(
            &u,
            "`df` rebound by a loop, `with` or `except` target"
        ));
    }

    #[test]
    fn test_should_flag_a_with_target_that_rebinds_the_frame() {
        let u = usage("with open(p) as df:\n    pass\n");
        assert!(has_other(
            &u,
            "`df` rebound by a loop, `with` or `except` target"
        ));
    }

    #[test]
    fn test_should_flag_an_except_target_that_rebinds_the_frame() {
        let u = usage("try:\n    pass\nexcept ValueError as df:\n    pass\n");
        assert!(has_other(
            &u,
            "`df` rebound by a loop, `with` or `except` target"
        ));
    }

    #[test]
    fn test_should_not_flag_a_loop_variable_unrelated_to_the_frame() {
        let u = usage("for row in rows:\n    pass\n");
        assert!(u.other_uses.is_empty());
    }

    #[test]
    fn test_should_visit_decorators_and_parameter_defaults_of_a_nested_def() {
        // arrange / act: both are evaluated in THIS scope when the `def` runs.
        let u = usage("@cache(df[\"a\"])\ndef f(x=df[\"b\"]):\n    pass\n");

        // assert
        assert_eq!(cols(&u), vec![("a", false), ("b", false)]);
    }

    #[test]
    fn test_should_visit_class_bases() {
        let u = usage("class C(make(df)):\n    pass\n");
        assert!(has_other(&u, "passed to `make()`"));
    }

    #[test]
    fn test_should_visit_a_lambda_default() {
        let u = usage("f = lambda x=df[\"a\"]: x\n");
        assert_eq!(cols(&u), vec![("a", false)]);
    }

    #[test]
    fn test_should_flag_dynamic_name_lookups() {
        for call in ["eval(s)", "exec(s)", "locals()", "globals()", "vars()"] {
            let u = usage(&format!("{call}\n"));
            assert!(
                u.other_uses
                    .iter()
                    .any(|o| o.description.ends_with("can reach any variable")),
                "{call}"
            );
        }
    }

    #[test]
    fn test_should_visit_a_nested_fstring_format_spec() {
        let u = usage("msg = f\"{x:{df}}\"\n");
        assert!(has_other(&u, "referenced"));
    }

    #[test]
    fn test_should_record_the_line_of_each_observation() {
        // arrange
        let source = "x = 1\nprint(df[\"a\"])\n\ndf.to_dict()\n";
        let module = parse_module(source).unwrap().into_syntax();
        let line_of = |offset: TextSize| source[..offset.to_usize()].matches('\n').count() + 1;

        // act
        let u = scan_usage("df", &module.body, &line_of);

        // assert
        assert_eq!(u.accesses[0].line, 2);
        assert_eq!(u.other_uses[0].line, 4);
    }

    #[test]
    fn test_should_track_every_alias_in_ever_tainted() {
        let u = usage("out = df\nout = other()\nalias = out\n");
        assert!(u.ever_tainted.contains("df"));
        assert!(u.ever_tainted.contains("out"));
        assert!(!u.ever_tainted.contains("alias"));
    }

    // --- first_nested_scope_mention / declares_global_or_nonlocal ---

    fn mention(source: &str) -> bool {
        let module = parse_module(source).unwrap().into_syntax();
        first_nested_scope_mention(&module.body, &HashSet::from(["df".to_string()])).is_some()
    }

    #[test]
    fn test_should_find_a_mention_inside_a_nested_function() {
        assert!(mention("def f():\n    return df[\"a\"]\n"));
    }

    #[test]
    fn test_should_find_a_mention_in_a_function_defined_before_the_origin() {
        // `body` is the whole scope: a function above the origin still runs later.
        assert!(mention("def f():\n    return df\ndf = load()\n"));
    }

    #[test]
    fn test_should_find_a_mention_inside_a_lambda_body() {
        assert!(mention("f = lambda: df\n"));
    }

    #[test]
    fn test_should_find_a_mention_inside_a_comprehension_body() {
        assert!(mention("out = [df[\"a\"] for x in xs]\n"));
    }

    #[test]
    fn test_should_find_a_mention_in_a_later_generator_clause() {
        assert!(mention("out = [y for x in xs for y in df]\n"));
    }

    #[test]
    fn test_should_find_a_mention_inside_a_class_body() {
        assert!(mention("class C:\n    x = df\n"));
    }

    #[test]
    fn test_should_not_count_the_outermost_comprehension_iterable() {
        // evaluated in the enclosing scope -- `scan_usage` already records it.
        assert!(!mention("out = sum(x for x in df[\"a\"])\n"));
    }

    #[test]
    fn test_should_not_count_decorators_defaults_or_class_bases() {
        assert!(!mention("@cache(df)\ndef f(x=df):\n    pass\n"));
        assert!(!mention("class C(make(df)):\n    pass\n"));
        assert!(!mention("f = lambda x=df: x\n"));
    }

    #[test]
    fn test_should_not_find_a_mention_of_an_unrelated_name() {
        assert!(!mention("def f():\n    return other\n"));
    }

    #[test]
    fn test_should_not_count_top_level_code() {
        assert!(!mention("print(df)\n"));
    }

    fn declares(source: &str) -> bool {
        let module = parse_module(source).unwrap().into_syntax();
        declares_global_or_nonlocal(&module.body, "df")
    }

    #[test]
    fn test_should_find_a_global_or_nonlocal_declaration() {
        assert!(declares("global df\n"));
        assert!(declares("if flag:\n    nonlocal df\n"));
    }

    #[test]
    fn test_should_ignore_a_declaration_of_another_name_or_in_a_nested_def() {
        assert!(!declares("global other\n"));
        assert!(!declares("def f():\n    global df\n"));
    }

    #[test]
    fn test_should_name_a_module_attribute_callee() {
        let u = usage("np.log(df)\n");
        assert_eq!(
            others(&u),
            vec![("passed to `np.log()`", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_name_a_keyword_argument_callee_too() {
        let u = usage("fit(data=df)\n");
        assert_eq!(
            others(&u),
            vec![("passed to `fit()`", false, Consumption::Unknown)]
        );
    }

    #[test]
    fn test_should_fall_back_to_a_generic_label_for_an_unnameable_callee() {
        let u = usage("registry[\"k\"](df)\n");
        assert_eq!(
            others(&u),
            vec![("passed to a call", false, Consumption::Unknown)]
        );
        let u = usage("a.b.c(df)\n");
        assert_eq!(
            others(&u),
            vec![("passed to a call", false, Consumption::Unknown)]
        );
    }
}
