//! What the callers of a function do with the frame it returns.
//!
//! A frame a function returns is only as wide as its callers need if every caller is seen
//! and each reads named columns and nothing else. This module observes one file: for each
//! callee name, the columns read from the result of every `x = name(...)` call, and whether
//! the name is also used in a way that hides a use (called other than as a plain assignment,
//! assigned inside a loop, handed on, passed around as a value, decorated). Names match by
//! bare name, so a call to an unrelated function of the same name only adds columns, never
//! removes them. The project-wide merge is `index::resolve_call_usage`.

use crate::column_usage::{
    blocks_narrowing, declares_global_or_nonlocal, first_nested_scope_mention, scan_usage_with,
    HelperResolver,
};
use crate::helpers::FileHelpers;
use crate::static_eval::{Analysis, ImportedModules};
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{ExceptHandler, Expr, ModModule, Stmt};
use std::collections::{BTreeSet, HashMap, HashSet};

/// What one file shows about the calls to a name.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Observation {
    /// Columns read from the result of the file's calls.
    pub(crate) columns: BTreeSet<String>,
    /// How many calls the file makes.
    pub(crate) calls: usize,
    /// Some use of the name or of a call's result isn't a read of named columns.
    pub(crate) unknown: bool,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct FileCalls {
    pub(crate) observations: HashMap<String, Observation>,
    /// Names of the functions and methods the file defines.
    pub(crate) defined: HashSet<String>,
}

fn callee_name(func: &Expr) -> Option<&str> {
    match func {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        _ => None,
    }
}

// Every call by callee name, and every other mention of a name that could be a function
// being handed around (`callbacks = [load]`, `map(load, ...)`, `obj.load`).
#[derive(Default)]
struct Mentions {
    calls: HashMap<String, usize>,
    other: HashSet<String>,
    defined: HashSet<String>,
}

impl<'a> Visitor<'a> for Mentions {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        // `from lib import load as fetch` calls `load` under another name.
        let aliased = match stmt {
            Stmt::Import(i) => Some(&i.names),
            Stmt::ImportFrom(i) => Some(&i.names),
            _ => None,
        };
        for alias in aliased.into_iter().flatten().filter(|a| a.asname.is_some()) {
            self.other.insert(alias.name.to_string());
        }
        if let Stmt::FunctionDef(def) = stmt {
            self.defined.insert(def.name.to_string());
            // A decorator (a route, a task, a fixture) may call it where this can't see.
            if !def.decorator_list.is_empty() {
                self.other.insert(def.name.to_string());
            }
        }
        ast_visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Call(call) => {
                if let Some(name) = callee_name(&call.func) {
                    *self.calls.entry(name.to_string()).or_default() += 1;
                }
                match &*call.func {
                    Expr::Name(_) => {}
                    Expr::Attribute(a) => self.visit_expr(&a.value),
                    other => self.visit_expr(other),
                }
                for arg in &call.arguments.args {
                    self.visit_expr(arg);
                }
                for keyword in &call.arguments.keywords {
                    self.visit_expr(&keyword.value);
                }
            }
            Expr::Name(n) => {
                self.other.insert(n.id.to_string());
            }
            Expr::Attribute(a) => {
                self.other.insert(a.attr.to_string());
                self.visit_expr(&a.value);
            }
            _ => ast_visitor::walk_expr(self, expr),
        }
    }
}

// The remainder of each enclosing block, innermost first, after `extra`.
fn following<'a>(extra: &[&'a [Stmt]], rest: &'a [Stmt], tails: &[&'a [Stmt]]) -> Vec<&'a [Stmt]> {
    let mut chain: Vec<&[Stmt]> = extra.to_vec();
    chain.push(rest);
    chain.extend_from_slice(tails);
    chain
}

// Finds each `x = name(...)` and reads off what the rest of its scope does with `x`.
struct Scanner<'h> {
    helpers: &'h dyn HelperResolver,
    // The module's own statements: a name assigned there can be imported and read by other
    // files, which this scan doesn't follow.
    module: &'h [Stmt],
    // Only calls to these names are worth following.
    defined: &'h HashSet<String>,
    // By callee name: the calls whose result was fully accounted for, and their columns.
    known: HashMap<String, Observation>,
}

impl Scanner<'_> {
    fn scope(&mut self, body: &[Stmt]) {
        self.block(body, &[], false, body);
    }

    fn class_body(&mut self, body: &[Stmt]) {
        for stmt in body {
            match stmt {
                Stmt::FunctionDef(def) => self.scope(&def.body),
                Stmt::ClassDef(inner) => self.class_body(&inner.body),
                _ => {}
            }
        }
    }

    fn block<'a>(
        &mut self,
        block: &'a [Stmt],
        tails: &[&'a [Stmt]],
        in_loop: bool,
        scope_body: &'a [Stmt],
    ) {
        for (i, stmt) in block.iter().enumerate() {
            let rest = &block[i + 1..];
            self.assignment(stmt, rest, tails, in_loop, scope_body);
            let after = |extra: &[&'a [Stmt]]| following(extra, rest, tails);
            match stmt {
                Stmt::FunctionDef(def) => self.scope(&def.body),
                Stmt::ClassDef(class) => self.class_body(&class.body),
                Stmt::If(s) => {
                    let chain = after(&[]);
                    self.block(&s.body, &chain, in_loop, scope_body);
                    for clause in &s.elif_else_clauses {
                        self.block(&clause.body, &chain, in_loop, scope_body);
                    }
                }
                Stmt::With(s) => self.block(&s.body, &after(&[]), in_loop, scope_body),
                Stmt::Match(s) => {
                    let chain = after(&[]);
                    for case in &s.cases {
                        self.block(&case.body, &chain, in_loop, scope_body);
                    }
                }
                Stmt::Try(s) => {
                    let handlers: Vec<&[Stmt]> = s
                        .handlers
                        .iter()
                        .map(|h| {
                            let ExceptHandler::ExceptHandler(h) = h;
                            h.body.as_slice()
                        })
                        .collect();
                    let finalbody = s.finalbody.as_slice();
                    let mut body_extra: Vec<&[Stmt]> = handlers.clone();
                    body_extra.push(s.orelse.as_slice());
                    body_extra.push(finalbody);
                    self.block(&s.body, &after(&body_extra), in_loop, scope_body);
                    let tail = after(&[finalbody]);
                    for handler in &handlers {
                        self.block(handler, &tail, in_loop, scope_body);
                    }
                    self.block(&s.orelse, &tail, in_loop, scope_body);
                    self.block(finalbody, &after(&[]), in_loop, scope_body);
                }
                Stmt::For(s) => {
                    let chain = after(&[]);
                    self.block(&s.body, &chain, true, scope_body);
                    self.block(&s.orelse, &chain, in_loop, scope_body);
                }
                Stmt::While(s) => {
                    let chain = after(&[]);
                    self.block(&s.body, &chain, true, scope_body);
                    self.block(&s.orelse, &chain, in_loop, scope_body);
                }
                _ => {}
            }
        }
    }

    fn assignment<'a>(
        &mut self,
        stmt: &'a Stmt,
        rest: &'a [Stmt],
        tails: &[&'a [Stmt]],
        in_loop: bool,
        scope_body: &'a [Stmt],
    ) {
        let (target, value) = match stmt {
            Stmt::Assign(a) => match (a.targets.as_slice(), &*a.value) {
                ([Expr::Name(target)], value) => (target, value),
                _ => return,
            },
            Stmt::AnnAssign(a) => match (&*a.target, a.value.as_deref()) {
                (Expr::Name(target), Some(value)) => (target, value),
                _ => return,
            },
            _ => return,
        };
        let Expr::Call(call) = value else {
            return;
        };
        // In a loop the result can be read before the next pass reassigns it.
        let Some(callee) =
            callee_name(&call.func).filter(|name| !in_loop && self.defined.contains(*name))
        else {
            return;
        };
        if std::ptr::eq(scope_body, self.module) {
            return;
        }
        let downstream = std::iter::once(rest)
            .chain(tails.iter().copied())
            .flat_map(|stmts| stmts.iter());
        let usage = scan_usage_with(target.id.as_str(), downstream, &|_| 0, self.helpers);
        if usage.other_uses.iter().any(blocks_narrowing)
            || first_nested_scope_mention(scope_body, &usage.ever_tainted).is_some()
            || usage
                .ever_tainted
                .iter()
                .any(|alias| declares_global_or_nonlocal(scope_body, alias))
        {
            return;
        }
        let seen = self.known.entry(callee.to_string()).or_default();
        seen.calls += 1;
        seen.columns
            .extend(usage.accesses.iter().map(|a| a.column.clone()));
    }
}

/// The names of the functions and methods `module` defines.
pub(crate) fn defined_names(module: &ModModule) -> HashSet<String> {
    let mut mentions = Mentions::default();
    for stmt in &module.body {
        mentions.visit_stmt(stmt);
    }
    mentions.defined
}

/// What `module` shows about the calls to each name it calls or mentions. Only calls to a
/// name in `defined` (what the project defines) have their results followed.
pub(crate) fn module_call_usage(module: &ModModule, defined: &HashSet<String>) -> FileCalls {
    let mut mentions = Mentions::default();
    for stmt in &module.body {
        mentions.visit_stmt(stmt);
    }
    let none = ImportedModules::new();
    let analysis = Analysis::build(module, &none);
    let imported = HashMap::new();
    let helpers = FileHelpers::new(&analysis, &imported).with_methods(module);
    let mut scanner = Scanner {
        helpers: &helpers,
        module: &module.body,
        defined,
        known: HashMap::new(),
    };
    scanner.scope(&module.body);

    let mut observations: HashMap<String, Observation> = HashMap::new();
    for (name, calls) in &mentions.calls {
        let known = scanner.known.remove(name);
        let accounted = known.as_ref().map_or(0, |k| k.calls);
        observations.insert(
            name.clone(),
            Observation {
                columns: known.map(|k| k.columns).unwrap_or_default(),
                calls: *calls,
                unknown: *calls != accounted || mentions.other.contains(name),
            },
        );
    }
    for name in &mentions.other {
        observations.entry(name.clone()).or_default().unknown = true;
    }
    FileCalls {
        observations,
        defined: mentions.defined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;

    fn observe(source: &str) -> FileCalls {
        let module = parse_module(source).unwrap().into_syntax();
        let mut defined = defined_names(&module);
        defined.extend(["load".to_string()]);
        module_call_usage(&module, &defined)
    }

    fn columns(calls: &FileCalls, name: &str) -> Vec<String> {
        calls.observations[name].columns.iter().cloned().collect()
    }

    #[test]
    fn test_should_collect_the_columns_read_from_each_assigned_call() {
        let calls = observe(
            "def f():\n    a = load()\n    print(a[\"x\"])\n    b = load()\n    return b[\"y\"]\n",
        );

        let seen = &calls.observations["load"];
        assert!(!seen.unknown);
        assert_eq!(seen.calls, 2);
        assert_eq!(columns(&calls, "load"), vec!["x", "y"]);
        assert!(calls.defined.contains("f"));
    }

    #[test]
    fn test_should_see_calls_through_methods_and_nested_blocks() {
        let calls = observe(
            "class S:\n    def f(self):\n        with ctx():\n            a = self.load()\n        return a[\"x\"]\n",
        );

        assert!(!calls.observations["load"].unknown);
        assert_eq!(columns(&calls, "load"), vec!["x"]);
    }

    #[test]
    fn test_should_not_know_a_call_whose_result_is_handed_on() {
        let calls = observe("def f():\n    a = load()\n    return a\n");

        assert!(calls.observations["load"].unknown);
    }

    #[test]
    fn test_should_not_know_a_call_that_is_not_a_plain_assignment() {
        let calls = observe("def f():\n    a = load()\n    return load()[\"x\"], a[\"y\"]\n");

        assert!(calls.observations["load"].unknown);
    }

    #[test]
    fn test_should_not_know_a_call_inside_a_loop() {
        let calls =
            observe("def f(xs):\n    for x in xs:\n        a = load(x)\n        print(a[\"y\"])\n");

        assert!(calls.observations["load"].unknown);
    }

    #[test]
    fn test_should_not_know_a_function_passed_around_as_a_value() {
        let calls = observe("def f():\n    a = load()\n    print(a[\"x\"])\n    register(load)\n");

        assert!(calls.observations["load"].unknown);
    }

    #[test]
    fn test_should_not_know_a_decorated_function() {
        let calls = observe("@route\ndef load():\n    return 1\n\n\ndef f():\n    a = load()\n    print(a[\"x\"])\n");

        assert!(calls.observations["load"].unknown);
    }

    #[test]
    fn test_should_read_through_a_helper_that_only_reads_columns() {
        let calls = observe(
            "def total(frame):\n    return frame[\"z\"]\n\n\ndef f():\n    a = load()\n    return total(a)\n",
        );

        assert!(!calls.observations["load"].unknown);
        assert_eq!(columns(&calls, "load"), vec!["z"]);
    }

    #[test]
    fn test_should_not_count_an_unrelated_assignment_of_something_else() {
        let calls = observe("def f():\n    a = 1\n    return a\n");

        assert!(!calls.observations.contains_key("load"));
    }

    #[test]
    fn test_should_not_know_a_function_another_file_imports_under_an_alias() {
        let calls = observe(
            "from lib import load as fetch\n\n\ndef f():\n    a = fetch()\n    return a[\"x\"]\n",
        );

        assert!(calls.observations["load"].unknown);
    }

    #[test]
    fn test_should_not_know_a_call_assigned_at_module_level() {
        let calls = observe("a = load()\nprint(a[\"x\"])\n");

        assert!(calls.observations["load"].unknown);
    }

    #[test]
    fn test_should_not_follow_calls_to_names_the_project_does_not_define() {
        let module = parse_module("def f():\n    a = other()\n    return a[\"x\"]\n")
            .unwrap()
            .into_syntax();

        let calls = module_call_usage(&module, &defined_names(&module));

        assert!(calls.observations["other"].unknown);
    }
}
