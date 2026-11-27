//! What a function does with each of its parameters, so passing a frame to it is not an
//! unknown use.
//!
//! A parameter has a summary only when every use of it inside the function is a read of a
//! named column (see `column_usage`); anything else -- returning it, calling a method that
//! may use every column, handing it to a function nothing is known about -- leaves it with
//! none, and the caller treats the call as unknown. Summaries follow the frame into other
//! functions of the same file, with cycles and depth bounded.

use crate::column_usage::{
    blocks_narrowing, declares_global_or_nonlocal, first_nested_scope_mention, scan_usage_with,
    HelperResolver, ParamUsage, CALLEE_IMPORTED, CALLEE_IMPORTED_NAME, CALLEE_MODULE_FUNCTION,
    CALLEE_OTHER_METHOD, CALLEE_OWN_METHOD, CALLEE_SAME_FILE, CALLEE_UNBOUND,
};
use crate::static_eval::{Analysis, CalleeBinding, ImportKind};
use ruff_python_ast::{self as ast, ModModule, Stmt};
use ruff_text_size::{Ranged, TextSize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

// Helpers calling helpers calling helpers; past this a chain is treated as unknown.
const MAX_DEPTH: usize = 8;

/// Resolves calls by bare name to functions defined in this file, and to functions
/// imported from other project files (whose summaries came from the index).
pub(crate) struct FileHelpers<'a> {
    analysis: &'a Analysis<'a>,
    imported: &'a HashMap<String, Vec<ParamUsage>>,
    // Every method of the file's classes by bare name; `None` once two classes define the
    // name, since `self` could then be an instance of either (or of a subclass).
    methods: HashMap<String, Option<&'a ast::StmtFunctionDef>>,
    memo: RefCell<HashMap<TextSize, Option<Vec<ParamUsage>>>>,
    active: RefCell<HashSet<TextSize>>,
}

fn collect_methods<'a>(
    body: &'a [Stmt],
    out: &mut HashMap<String, Option<&'a ast::StmtFunctionDef>>,
) {
    for stmt in body {
        let Stmt::ClassDef(class) = stmt else {
            continue;
        };
        for member in &class.body {
            if let Stmt::FunctionDef(def) = member {
                out.entry(def.name.to_string())
                    .and_modify(|existing| *existing = None)
                    .or_insert(Some(def));
            }
        }
        collect_methods(&class.body, out);
    }
}

impl<'a> FileHelpers<'a> {
    pub(crate) fn new(
        analysis: &'a Analysis<'a>,
        imported: &'a HashMap<String, Vec<ParamUsage>>,
    ) -> Self {
        FileHelpers {
            analysis,
            imported,
            methods: HashMap::new(),
            memo: RefCell::new(HashMap::new()),
            active: RefCell::new(HashSet::new()),
        }
    }

    /// Also resolves `self.<method>(...)` calls to the methods of `module`'s classes.
    pub(crate) fn with_methods(mut self, module: &'a ModModule) -> Self {
        collect_methods(&module.body, &mut self.methods);
        self
    }

    /// The summary of each parameter of `def` that has one.
    pub(crate) fn summarize(&self, def: &ast::StmtFunctionDef) -> Option<Vec<ParamUsage>> {
        let key = def.range().start();
        if let Some(done) = self.memo.borrow().get(&key) {
            return done.clone();
        }
        // A function reaching itself, or a chain too deep to be worth following, is unknown.
        // Not memoized: the answer depends on where in a chain it was asked.
        if self.active.borrow().contains(&key) || self.active.borrow().len() >= MAX_DEPTH {
            return None;
        }
        self.active.borrow_mut().insert(key);
        let summary = self.compute(def);
        self.active.borrow_mut().remove(&key);
        self.memo.borrow_mut().insert(key, summary.clone());
        summary
    }

    fn compute(&self, def: &ast::StmtFunctionDef) -> Option<Vec<ParamUsage>> {
        let parameters = &def.parameters;
        let positional = parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .enumerate();
        let keyword_only = parameters.kwonlyargs.iter().map(|p| (usize::MAX, p));
        let mut summaries = Vec::new();
        for (position, parameter) in positional.chain(keyword_only) {
            let name = parameter.parameter.name.as_str();
            let usage = scan_usage_with(name, def.body.iter(), &|_| 0, self);
            if usage.other_uses.iter().any(blocks_narrowing)
                || first_nested_scope_mention(&def.body, &usage.ever_tainted).is_some()
                || usage
                    .ever_tainted
                    .iter()
                    .any(|alias| declares_global_or_nonlocal(&def.body, alias))
            {
                continue;
            }
            let mut accesses: Vec<(String, bool)> = Vec::new();
            for access in &usage.accesses {
                match accesses.iter_mut().find(|(c, _)| *c == access.column) {
                    Some((_, conditional)) => *conditional &= access.conditional,
                    None => accesses.push((access.column.clone(), access.conditional)),
                }
            }
            summaries.push(ParamUsage {
                position,
                name: name.to_string(),
                accesses,
            });
        }
        Some(summaries)
    }
}

impl HelperResolver for FileHelpers<'_> {
    fn is_unshadowed_builtin(&self, callee: &ast::ExprName) -> bool {
        self.analysis.is_unbound_name(callee)
    }

    fn method_usage_of(&self, method: &str) -> Option<Vec<ParamUsage>> {
        let def = (*self.methods.get(method)?)?;
        if !def.decorator_list.is_empty() || def.is_async {
            return None;
        }
        let summaries = self.summarize(def)?;
        // `self` takes the first position, so a caller's arguments are one place earlier.
        Some(
            summaries
                .into_iter()
                .filter(|p| p.position != 0)
                .map(|mut p| {
                    if p.position != usize::MAX {
                        p.position -= 1;
                    }
                    p
                })
                .collect(),
        )
    }

    fn callee_class(&self, func: &ast::Expr) -> Option<&'static str> {
        match func {
            ast::Expr::Name(name) => {
                if self.analysis.is_unbound_name(name) {
                    return Some(CALLEE_UNBOUND);
                }
                match self.analysis.callee_binding(name)? {
                    CalleeBinding::Function(_) => Some(CALLEE_SAME_FILE),
                    CalleeBinding::Imported { .. } => Some(CALLEE_IMPORTED),
                }
            }
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(base) = &*attribute.value else {
                    return None;
                };
                Some(match self.analysis.import_kind(base) {
                    Some(ImportKind::Module) => CALLEE_MODULE_FUNCTION,
                    Some(ImportKind::From) => CALLEE_IMPORTED_NAME,
                    None if matches!(base.id.as_str(), "self" | "cls") => CALLEE_OWN_METHOD,
                    None => CALLEE_OTHER_METHOD,
                })
            }
            _ => None,
        }
    }

    fn usage_of(&self, callee: &ast::ExprName) -> Option<Vec<ParamUsage>> {
        match self.analysis.callee_binding(callee)? {
            CalleeBinding::Function(def) => self.summarize(def),
            CalleeBinding::Imported { local } => self.imported.get(&local).cloned(),
        }
    }
}

/// Summaries for every module-level function of `module`, by name, for the index. Only
/// functions with at least one summarized parameter are included.
#[cfg(test)]
pub(crate) fn module_param_usages(module: &ModModule) -> HashMap<String, Vec<ParamUsage>> {
    let none = crate::static_eval::ImportedModules::new();
    module_param_usages_in(&Analysis::build(module, &none), module)
}

/// `module_param_usages` for an analysis already built from `module` with no imports.
pub(crate) fn module_param_usages_in(
    analysis: &Analysis,
    module: &ModModule,
) -> HashMap<String, Vec<ParamUsage>> {
    let imported = HashMap::new();
    let helpers = FileHelpers::new(analysis, &imported);
    let mut out = HashMap::new();
    for stmt in &module.body {
        let Stmt::FunctionDef(def) = stmt else {
            continue;
        };
        let Some(def) = analysis.single_module_function(def.name.as_str()) else {
            continue;
        };
        if let Some(summaries) = helpers.summarize(def).filter(|s| !s.is_empty()) {
            out.insert(def.name.to_string(), summaries);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column_usage::scan_usage_with;
    use crate::static_eval::ImportedModules;
    use ruff_python_parser::parse_module;

    // Scans the statements after the first assignment of `df`, with helpers from `source`.
    fn scan(source: &str) -> crate::column_usage::ColumnUsage {
        let module = parse_module(source).unwrap().into_syntax();
        let none = ImportedModules::new();
        let analysis = Analysis::build(&module, &none);
        let imported = HashMap::new();
        let helpers = FileHelpers::new(&analysis, &imported).with_methods(&module);
        let origin = module
            .body
            .iter()
            .position(|s| matches!(s, Stmt::Assign(a) if matches!(a.targets.as_slice(), [ast::Expr::Name(n)] if n.id.as_str() == "df")))
            .unwrap();
        scan_usage_with("df", &module.body[origin + 1..], &|_| 1, &helpers)
    }

    fn columns(usage: &crate::column_usage::ColumnUsage) -> Vec<(&str, bool)> {
        usage
            .accesses
            .iter()
            .map(|a| (a.column.as_str(), a.conditional))
            .collect()
    }

    #[test]
    fn test_should_count_the_columns_a_helper_reads() {
        let source = "def total(frame):\n    return frame[\"a\"].sum() + frame[\"b\"].sum()\n\n\ndf = load()\nprint(total(df))\nprint(df[\"c\"])\n";

        let usage = scan(source);

        assert!(usage.other_uses.is_empty(), "{:?}", usage.other_uses);
        assert_eq!(
            columns(&usage),
            vec![("a", false), ("b", false), ("c", false)]
        );
        assert_eq!(usage.accesses[0].via.as_deref(), Some("total"));
    }

    #[test]
    fn test_should_count_the_columns_a_method_called_on_self_reads() {
        // `self` is not an argument, so the frame is the call's first.
        let source = "class S:\n    def total(self, n, frame):\n        return frame[\"a\"]\n\n\ndf = load()\nprint(self.total(1, df))\nprint(self.total(1, frame=df))\n";

        let usage = scan(source);

        assert!(usage.other_uses.is_empty(), "{:?}", usage.other_uses);
        assert_eq!(columns(&usage), vec![("a", false), ("a", false)]);
        assert_eq!(usage.accesses[0].via.as_deref(), Some("self.total"));
    }

    #[test]
    fn test_should_not_know_a_method_two_classes_define() {
        let source = "class A:\n    def total(self, frame):\n        return frame[\"a\"]\n\n\nclass B(A):\n    def total(self, frame):\n        return frame[\"b\"]\n\n\ndf = load()\nprint(self.total(df))\n";

        let usage = scan(source);

        assert!(usage.accesses.is_empty());
        assert_eq!(usage.other_uses.len(), 1);
    }

    #[test]
    fn test_should_not_know_a_decorated_method_or_one_that_returns_the_frame() {
        let source = "class S:\n    @staticmethod\n    def read(frame):\n        return frame[\"a\"]\n\n    def back(self, frame):\n        return frame\n\n\ndf = load()\nprint(self.read(df))\nprint(self.back(df))\n";

        let usage = scan(source);

        assert!(usage.accesses.is_empty());
        assert_eq!(usage.other_uses.len(), 2);
    }

    #[test]
    fn test_should_match_a_keyword_argument_by_parameter_name() {
        let source = "def total(n, frame):\n    return frame[\"a\"]\n\n\ndf = load()\nprint(total(1, frame=df))\nprint(total(1, df))\n";

        let usage = scan(source);

        assert!(usage.other_uses.is_empty());
        assert_eq!(columns(&usage), vec![("a", false), ("a", false)]);
    }

    #[test]
    fn test_should_mark_a_helpers_conditional_read_conditional() {
        let source = "def f(d):\n    if d.empty:\n        return 0\n    return d[\"a\"]\n\n\ndf = load()\nf(df)\n";

        let usage = scan(source);

        assert_eq!(columns(&usage), vec![("a", false)]);
        let guarded =
            "def f(d, flag):\n    if flag:\n        return d[\"a\"]\n\n\ndf = load()\nf(df, 1)\n";
        assert_eq!(columns(&scan(guarded)), vec![("a", true)]);
    }

    #[test]
    fn test_should_follow_a_chain_of_helpers() {
        let source = "def inner(x):\n    return x[\"a\"]\n\n\ndef outer(y):\n    return inner(y) + y[\"b\"]\n\n\ndf = load()\nouter(df)\n";

        let usage = scan(source);

        assert!(usage.other_uses.is_empty());
        assert_eq!(columns(&usage), vec![("a", false), ("b", false)]);
    }

    #[test]
    fn test_should_not_summarize_a_helper_that_uses_the_frame_in_an_unknown_way() {
        for body in [
            "return d",
            "return d.dropna()",
            "d[\"a\"] = 1\n    return d.groupby(\"k\")",
            "other(d)",
            "global G\n    G = d",
            "store.append(d)",
            "return [d]",
            "return (lambda: d[\"a\"])()",
        ] {
            let source = format!("def f(d):\n    {body}\n\n\ndf = load()\nf(df)\n");

            let usage = scan(&source);

            assert!(
                usage.other_uses.iter().any(blocks_narrowing),
                "{body}: {:?}",
                usage.other_uses
            );
        }
    }

    #[test]
    fn test_should_not_follow_a_recursive_or_mutually_recursive_helper() {
        for source in [
            "def f(d):\n    return f(d)\n\n\ndf = load()\nf(df)\n",
            "def f(d):\n    return g(d)\n\n\ndef g(d):\n    return f(d)\n\n\ndf = load()\nf(df)\n",
        ] {
            let usage = scan(source);

            assert!(usage.other_uses.iter().any(blocks_narrowing), "{source}");
        }
    }

    #[test]
    fn test_should_not_trust_a_name_that_is_not_a_single_plain_function() {
        for header in [
            "@cache\ndef f(d):\n    return d[\"a\"]",
            "async def f(d):\n    return d[\"a\"]",
            "def f(d):\n    return d[\"a\"]\n\n\ndef f(d):\n    return d[\"b\"]",
            "def f(d):\n    return d[\"a\"]\n\n\nf = other",
        ] {
            let source = format!("{header}\n\n\ndf = load()\nf(df)\n");

            let usage = scan(&source);

            assert!(usage.other_uses.iter().any(blocks_narrowing), "{header}");
        }
    }

    #[test]
    fn test_should_not_count_a_helpers_result_as_the_frame() {
        // `out` is a Series here; treating `out["x"]` as a column of df would be wrong
        let source =
            "def col(d):\n    return d[\"a\"]\n\n\ndf = load()\nout = col(df)\nprint(out[\"x\"])\n";

        let usage = scan(source);

        assert_eq!(columns(&usage), vec![("a", false)]);
    }

    #[test]
    fn test_should_still_count_the_result_of_an_unsummarized_call_as_the_frame() {
        let source = "df = load()\nout = unknown(df)\nprint(out[\"x\"])\n";

        let usage = scan(source);

        assert!(usage.other_uses.iter().any(blocks_narrowing));
        assert!(columns(&usage).contains(&("x", false)));
    }

    #[test]
    fn test_should_not_count_a_helper_read_of_a_column_the_caller_created() {
        let source = "def f(d):\n    return d[\"total\"]\n\n\ndf = load()\ndf[\"total\"] = df[\"a\"]\nf(df)\n";

        let usage = scan(source);

        assert_eq!(columns(&usage), vec![("a", false)]);
    }

    #[test]
    fn test_should_summarize_every_module_function_for_the_index() {
        let module =
            parse_module("def f(d, n):\n    return d[\"a\"]\n\n\ndef g(d):\n    return d\n")
                .unwrap()
                .into_syntax();

        let usages = module_param_usages(&module);

        let f = &usages["f"];
        assert!(f
            .iter()
            .any(|p| p.name == "d" && p.accesses == vec![("a".to_string(), false)]));
        assert!(f.iter().any(|p| p.name == "n" && p.accesses.is_empty()));
        assert!(!usages.contains_key("g"));
    }
}
