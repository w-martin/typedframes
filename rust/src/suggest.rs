//! Suggestions for an untyped DataFrame origin, built from how the rest of its scope
//! uses it (see `column_usage`).
//!
//! The one suggestion is a column selection, which gives the checker an exact schema:
//! `usecols=`/`columns=` added to a plain `read_csv`/`read_parquet` call, or else
//! `df = df[["a", "b"]]` (pandas) / `df = df.select("a", "b")` (polars) on its own line
//! after the load. It rewrites real code, so it is made only when every use of the
//! frame is visible and is a read of a named column; anything else is withheld with a
//! reason, since dropping a column the program still reads breaks it at runtime.
//!
//! Output is byte-range edits plus a rendered diff; `apply` is what writes them.

use crate::column_usage::{ColumnUsage, OtherUse};
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::HashMap;

/// One byte-range replacement against the ORIGINAL source (`start == end` inserts).
#[derive(Debug, Clone, Serialize)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    /// The site's stable two-word name; filled in once all of a file's sites are known.
    pub phrase: String,
    pub kind: &'static str,
    pub line: usize,
    pub col: usize,
    pub var: String,
    /// `"safe"` when nothing about the change needs a human's judgement, `"review"`
    /// when it is probably right but depends on something this scan can't see (see
    /// `notes`).
    pub tier: &'static str,
    pub columns: Vec<String>,
    pub reason: String,
    pub notes: Vec<String>,
    pub edits: Vec<Edit>,
    pub diff: String,
}

/// A site that got no suggestion, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Withheld {
    pub phrase: String,
    pub line: usize,
    pub col: usize,
    pub var: String,
    pub reason: String,
}

pub(crate) struct Origin<'a> {
    pub(crate) libraries: &'a LibraryContext,
    /// The variable's own annotation, when it has one (`df: pd.DataFrame = ...`).
    pub(crate) annotation: Option<&'a Expr>,
    pub(crate) source: &'a str,
    pub(crate) display_path: &'a str,
    /// The expression assigned to the variable (`None` for a bare annotation).
    pub(crate) value: Option<&'a Expr>,
    pub(crate) var: &'a str,
    pub(crate) line: usize,
    pub(crate) col: usize,
    /// Byte range of the whole assignment statement.
    pub(crate) stmt_start: usize,
    pub(crate) stmt_end: usize,
    /// A local of a function rather than a module-level name. A module-level frame can
    /// be imported by other modules, which this scan can't see.
    pub(crate) function_local: bool,
    /// Local names (and parameters) of the enclosing function that are certainly an
    /// instance of a class, by class name: `svc = Service()`, `def f(svc: Service)`.
    pub(crate) receivers: &'a HashMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Library {
    Pandas,
    Polars,
}

impl Library {
    /// The name stored in the index.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Library::Pandas => "pandas",
            Library::Polars => "polars",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Library> {
        match name {
            "pandas" => Some(Library::Pandas),
            "polars" => Some(Library::Polars),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ModuleKind {
    Library(Library),
    Connectorx,
}

/// What a file's own imports and function signatures say about which library a frame
/// belongs to. Built once per file.
#[derive(Default, Debug)]
pub(crate) struct LibraryContext {
    // Local name of an imported module: `import pandas as pd` -> pd.
    modules: HashMap<String, ModuleKind>,
    // Local name of something imported from pandas/polars: `from pandas import read_csv`.
    names: HashMap<String, Library>,
    // Functions whose return annotation says pandas or polars, by bare name: this file's
    // own, plus those imported from other project files (from the index).
    functions: HashMap<String, Library>,
    // Methods of this file's classes whose return annotation says pandas or polars, by
    // `(class, method)`: a call on a receiver known to be an instance of the class.
    class_methods: HashMap<(String, String), Library>,
}

impl LibraryContext {
    /// `imported_functions`: libraries of functions this file imports from other files.
    pub(crate) fn new(
        body: &[Stmt],
        imported_functions: &HashMap<String, Library>,
    ) -> LibraryContext {
        let mut context = LibraryContext {
            functions: imported_functions.clone(),
            ..LibraryContext::default()
        };
        context.collect_imports(body);
        // A name defined twice with different libraries can't be trusted either way.
        let mut own: HashMap<String, Option<Library>> = HashMap::new();
        collect_function_returns(body, &context, &mut own);
        for (name, library) in own {
            match library {
                Some(library) => context.functions.insert(name, library),
                None => context.functions.remove(&name),
            };
        }
        context.class_methods = class_method_libraries(body, &context);
        context
    }

    #[cfg(test)]
    fn extend(&mut self, body: &[Stmt]) {
        self.collect_imports(body);
        let mut own: HashMap<String, Option<Library>> = HashMap::new();
        collect_function_returns(body, self, &mut own);
        for (name, library) in own {
            match library {
                Some(library) => self.functions.insert(name, library),
                None => self.functions.remove(&name),
            };
        }
        self.class_methods = class_method_libraries(body, self);
    }

    fn collect_imports(&mut self, body: &[Stmt]) {
        for stmt in body {
            match stmt {
                Stmt::Import(import) => {
                    for alias in &import.names {
                        let local = alias.asname.as_ref().unwrap_or(&alias.name).as_str();
                        let kind = match alias.name.as_str() {
                            "pandas" => Some(ModuleKind::Library(Library::Pandas)),
                            "polars" => Some(ModuleKind::Library(Library::Polars)),
                            "connectorx" => Some(ModuleKind::Connectorx),
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            self.modules.insert(local.to_string(), kind);
                        }
                    }
                }
                Stmt::ImportFrom(import) if import.level == 0 => {
                    let library = match import.module.as_ref().map(|m| m.as_str()) {
                        Some("pandas") => Some(Library::Pandas),
                        Some("polars") => Some(Library::Polars),
                        _ => None,
                    };
                    if let Some(library) = library {
                        for alias in &import.names {
                            let local = alias.asname.as_ref().unwrap_or(&alias.name).as_str();
                            self.names.insert(local.to_string(), library);
                        }
                    }
                }
                Stmt::FunctionDef(f) => self.collect_imports(&f.body),
                Stmt::ClassDef(c) => self.collect_imports(&c.body),
                Stmt::If(i) => {
                    self.collect_imports(&i.body);
                    for clause in &i.elif_else_clauses {
                        self.collect_imports(&clause.body);
                    }
                }
                Stmt::Try(t) => {
                    self.collect_imports(&t.body);
                    self.collect_imports(&t.orelse);
                }
                _ => {}
            }
        }
    }

    /// The library an annotation names (`pd.DataFrame`, `pl.LazyFrame`, an imported
    /// `DataFrame`), unwrapping `Optional`/`Annotated`/`X | None`.
    fn annotation_library(&self, annotation: &Expr) -> Option<Library> {
        match annotation {
            Expr::Attribute(attribute)
                if matches!(attribute.attr.as_str(), "DataFrame" | "LazyFrame") =>
            {
                let Expr::Name(base) = &*attribute.value else {
                    return None;
                };
                match self.modules.get(base.id.as_str())? {
                    ModuleKind::Library(library) => Some(*library),
                    ModuleKind::Connectorx => None,
                }
            }
            Expr::Name(name) if matches!(name.id.as_str(), "DataFrame" | "LazyFrame") => {
                self.names.get(name.id.as_str()).copied()
            }
            Expr::Subscript(subscript) => {
                let wrapper = match &*subscript.value {
                    Expr::Name(n) => n.id.as_str(),
                    Expr::Attribute(a) => a.attr.as_str(),
                    _ => return None,
                };
                if !matches!(wrapper, "Annotated" | "Optional") {
                    return None;
                }
                match &*subscript.slice {
                    Expr::Tuple(items) => self.annotation_library(items.elts.first()?),
                    single => self.annotation_library(single),
                }
            }
            Expr::BinOp(binop) => self
                .annotation_library(&binop.left)
                .or_else(|| self.annotation_library(&binop.right)),
            _ => None,
        }
    }
}

// Every function (and method) of `body` with a return annotation that names a library,
// keyed by bare name; `None` once two definitions of the name disagree.
fn collect_function_returns(
    body: &[Stmt],
    context: &LibraryContext,
    out: &mut HashMap<String, Option<Library>>,
) {
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(function) => {
                let library = function
                    .returns
                    .as_deref()
                    .and_then(|r| context.annotation_library(r));
                out.entry(function.name.to_string())
                    .and_modify(|existing| {
                        if *existing != library {
                            *existing = None;
                        }
                    })
                    .or_insert(library);
                collect_function_returns(&function.body, context, out);
            }
            Stmt::ClassDef(class) => collect_function_returns(&class.body, context, out),
            _ => {}
        }
    }
}

// Every method of every class in `body` with a return annotation that names a library,
// keyed by `(class, method)`; `None` once two definitions of the pair disagree, or the
// method is async or decorated (other than `staticmethod`/`classmethod`), so what it returns
// can't be read from its annotation. `bases` collects `(subclass, base)` pairs by name.
fn collect_class_method_returns(
    body: &[Stmt],
    context: &LibraryContext,
    out: &mut HashMap<(String, String), Option<Library>>,
    bases: &mut Vec<(String, String)>,
) {
    for stmt in body {
        match stmt {
            Stmt::ClassDef(class) => {
                for base in class.bases() {
                    if let Expr::Name(base) = base {
                        bases.push((class.name.to_string(), base.id.to_string()));
                    }
                }
                for member in &class.body {
                    let Stmt::FunctionDef(method) = member else {
                        continue;
                    };
                    let plain = !method.is_async
                        && method.decorator_list.iter().all(|d| {
                            matches!(&d.expression, Expr::Name(n)
                                if matches!(n.id.as_str(), "staticmethod" | "classmethod"))
                        });
                    let library = method
                        .returns
                        .as_deref()
                        .and_then(|r| context.annotation_library(r))
                        .filter(|_| plain);
                    out.entry((class.name.to_string(), method.name.to_string()))
                        .and_modify(|existing| {
                            if *existing != library {
                                *existing = None;
                            }
                        })
                        .or_insert(library);
                }
                collect_class_method_returns(&class.body, context, out, bases);
            }
            Stmt::FunctionDef(function) => {
                collect_class_method_returns(&function.body, context, out, bases);
            }
            _ => {}
        }
    }
}

// The library each `(class, method)` of `body` returns, leaving out a method that a subclass
// in the file overrides with a different answer: a receiver annotated with the base class
// may be the subclass.
fn class_method_libraries(
    body: &[Stmt],
    context: &LibraryContext,
) -> HashMap<(String, String), Library> {
    let mut methods: HashMap<(String, String), Option<Library>> = HashMap::new();
    let mut bases: Vec<(String, String)> = Vec::new();
    collect_class_method_returns(body, context, &mut methods, &mut bases);
    for (subclass, base) in &bases {
        let overrides: Vec<(String, Option<Library>)> = methods
            .iter()
            .filter(|((class, _), _)| class == subclass)
            .map(|((_, method), library)| (method.clone(), *library))
            .collect();
        for (method, library) in overrides {
            if let Some(inherited) = methods.get_mut(&(base.clone(), method)) {
                if *inherited != library {
                    *inherited = None;
                }
            }
        }
    }
    methods
        .into_iter()
        .filter_map(|(key, library)| Some((key, library?)))
        .collect()
}

/// The library each function of `module` returns, by name -- what the index stores so
/// other files can resolve a call to it.
pub(crate) fn function_libraries(module: &ModModule) -> HashMap<String, Library> {
    LibraryContext::new(&module.body, &HashMap::new()).functions
}

/// `Ok` with a suggestion, or `Err` with the reason none could be made.
pub(crate) fn suggest_selection(
    origin: &Origin,
    usage: &ColumnUsage,
) -> Result<Suggestion, String> {
    let library = library_of(origin)?;
    let in_call = in_call_selection(origin, library);
    let insertion = insertion_point(origin)?;

    let blockers: Vec<&OtherUse> = usage
        .other_uses
        .iter()
        .filter(|u| crate::column_usage::blocks_narrowing(u))
        .collect();
    if let Some(first) = blockers.first() {
        let more = match blockers.len() - 1 {
            0 => String::new(),
            n => format!(", and {n} more"),
        };
        let callee = first
            .callee
            .map_or_else(String::new, |class| format!(", {class}"));
        return Err(format!(
            "can't see everything done with it: {}{callee} (line {}){more}",
            first.description, first.line
        ));
    }
    if usage.accesses.is_empty() {
        return Err("no column is read from it later in this scope".to_string());
    }

    let mut columns: Vec<String> = Vec::new();
    for access in &usage.accesses {
        if !columns.contains(&access.column) {
            columns.push(access.column.clone());
        }
    }

    let mut notes: Vec<String> = Vec::new();
    for column in &columns {
        let mut reads = usage.accesses.iter().filter(|a| &a.column == column);
        if reads.clone().all(|a| a.conditional) {
            let line = reads.next().map_or(0, |a| a.line);
            notes.push(format!(
                "'{column}' is only read conditionally (line {line}); if the data lacks it, \
                 the selection would now fail"
            ));
        }
    }
    let outputs: Vec<&OtherUse> = usage
        .other_uses
        .iter()
        .filter(|u| {
            u.description
                .starts_with(crate::column_usage::OUTPUT_USE_PREFIX)
        })
        .collect();
    if let Some(first) = outputs.first() {
        notes.push(format!(
            "the frame is displayed (line {}); the output will show only the selected columns",
            first.line
        ));
    }
    if library == Library::Pandas && usage.wrote_columns && in_call.is_none() {
        notes.push(
            "columns are assigned into the frame later; on older pandas, assigning into a \
             column selection can raise SettingWithCopyWarning"
                .to_string(),
        );
    }
    let mut methods: Vec<&str> = Vec::new();
    for via in usage.accesses.iter().filter_map(|a| a.via.as_deref()) {
        if via.starts_with("self.") && !methods.contains(&via) {
            methods.push(via);
        }
    }
    if !methods.is_empty() {
        let names: Vec<String> = methods.iter().map(|m| format!("`{m}()`")).collect();
        notes.push(format!(
            "assumes {} is not overridden in a subclass in another file, which this scan \
             can't see",
            names.join(", ")
        ));
    }
    if let Some(via) = usage
        .accesses
        .iter()
        .filter_map(|a| a.via.as_deref())
        .find(|v| v.starts_with("callers of "))
    {
        notes.push(format!(
            "the frame is returned, so these columns are the ones read by the {via} seen \
             under the scanned path; callers outside it, or reaching the function dynamically, \
             can't be seen"
        ));
    }
    if !origin.function_local {
        notes.push(
            "module-level variable: another module may import it and read columns this scan \
             can't see"
                .to_string(),
        );
    }

    let quoted: Vec<String> = columns
        .iter()
        .map(|c| serde_json::to_string(c).unwrap_or_else(|_| format!("\"{c}\"")))
        .collect();
    let (edit, new_source, kind) = match &in_call {
        Some(site) => {
            let text = format!(", {}=[{}]", site.keyword, quoted.join(", "));
            let mut new_source = String::with_capacity(origin.source.len() + text.len());
            new_source.push_str(&origin.source[..site.offset]);
            new_source.push_str(&text);
            new_source.push_str(&origin.source[site.offset..]);
            let edit = Edit {
                start: site.offset,
                end: site.offset,
                replacement: text,
            };
            (edit, new_source, site.keyword)
        }
        None => {
            let expression = match library {
                Library::Pandas => format!("{}[[{}]]", origin.var, quoted.join(", ")),
                Library::Polars => format!("{}.select({})", origin.var, quoted.join(", ")),
            };
            let replacement = format!(
                "{}{} = {expression}{}",
                insertion.indent, origin.var, insertion.newline
            );
            let mut new_source = String::with_capacity(origin.source.len() + replacement.len());
            new_source.push_str(&origin.source[..insertion.offset]);
            if insertion.leading_newline {
                new_source.push('\n');
            }
            new_source.push_str(&replacement);
            new_source.push_str(&origin.source[insertion.offset..]);
            let edit_text = if insertion.leading_newline {
                format!("\n{replacement}")
            } else {
                replacement
            };
            let edit = Edit {
                start: insertion.offset,
                end: insertion.offset,
                replacement: edit_text,
            };
            (edit, new_source, "select")
        }
    };

    let mut lines: Vec<usize> = usage.accesses.iter().map(|a| a.line).collect();
    lines.sort_unstable();
    lines.dedup();
    let mut helpers: Vec<&str> = usage
        .accesses
        .iter()
        .filter_map(|a| a.via.as_deref())
        .collect();
    helpers.sort_unstable();
    helpers.dedup();
    let (callers, helpers): (Vec<&str>, Vec<&str>) = helpers
        .into_iter()
        .partition(|v| v.starts_with("callers of "));
    let through = if helpers.is_empty() {
        String::new()
    } else {
        format!(
            "; some are read inside {}",
            helpers
                .iter()
                .map(|h| format!("`{h}()`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let through = if callers.is_empty() {
        through
    } else {
        format!("{through}; read by the {}", callers.join(", "))
    };
    let reason = format!(
        "only {} {} read later (line{} {}{through})",
        columns
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(", "),
        if columns.len() == 1 { "is" } else { "are" },
        if lines.len() == 1 { "" } else { "s" },
        lines
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    );

    Ok(Suggestion {
        phrase: String::new(),
        kind,
        line: origin.line,
        col: origin.col,
        var: origin.var.to_string(),
        tier: if notes.is_empty() { "safe" } else { "review" },
        columns,
        reason,
        notes,
        edits: vec![edit],
        diff: render_unified_diff(origin.display_path, origin.source, &new_source),
    })
}

/// Where to add a column list to the load call itself.
struct InCall {
    /// The keyword to add: `usecols` or `columns`.
    keyword: &'static str,
    /// Just after the call's last argument.
    offset: usize,
}

// `pd.read_csv(path)` and the like can take the column list directly, which avoids
// reading the other columns at all. Only for a bare `read_csv`/`read_parquet` call whose
// options can't refer to a column outside the list: one positional path plus keywords
// from a short list of harmless ones.
fn in_call_selection(origin: &Origin, library: Library) -> Option<InCall> {
    let Some(Expr::Call(call)) = origin.value else {
        return None;
    };
    let name = match &*call.func {
        Expr::Attribute(a) => match &*a.value {
            Expr::Name(base)
                if matches!(
                    origin.libraries.modules.get(base.id.as_str()),
                    Some(ModuleKind::Library(l)) if *l == library
                ) =>
            {
                a.attr.as_str()
            }
            _ => return None,
        },
        Expr::Name(n) if origin.libraries.names.get(n.id.as_str()) == Some(&library) => {
            n.id.as_str()
        }
        _ => return None,
    };
    let (keyword, harmless): (&'static str, &[&str]) = match (library, name) {
        (Library::Pandas, "read_csv") => (
            "usecols",
            &[
                "sep",
                "delimiter",
                "encoding",
                "encoding_errors",
                "nrows",
                "low_memory",
                "engine",
                "compression",
                "na_values",
                "keep_default_na",
                "decimal",
                "thousands",
            ],
        ),
        (Library::Pandas, "read_parquet") => ("columns", &["engine", "dtype_backend"]),
        (Library::Polars, "read_csv") => (
            "columns",
            &[
                "separator",
                "encoding",
                "n_rows",
                "infer_schema_length",
                "null_values",
                "ignore_errors",
            ],
        ),
        (Library::Polars, "read_parquet") => ("columns", &["n_rows", "parallel", "use_pyarrow"]),
        _ => return None,
    };
    let arguments = &call.arguments;
    if arguments.args.len() != 1 || matches!(arguments.args[0], Expr::Starred(_)) {
        return None;
    }
    let all_harmless = arguments.keywords.iter().all(|k| {
        k.arg
            .as_ref()
            .is_some_and(|a| harmless.contains(&a.as_str()))
    });
    if !all_harmless {
        return None;
    }
    let last_end = arguments
        .keywords
        .iter()
        .map(|k| k.range().end())
        .chain(arguments.args.iter().map(|a| a.range().end()))
        .max()?
        .to_usize();
    Some(InCall {
        keyword,
        offset: after_closing_parens(origin.source, last_end, call.range().end().to_usize()),
    })
}

// An argument written as `(path)` ends before its own closing parenthesis, so step over
// those; the call's own closing parenthesis is the last character before `call_end`.
fn after_closing_parens(source: &str, from: usize, call_end: usize) -> usize {
    let mut position = from;
    loop {
        let rest = &source[position..call_end.saturating_sub(1).max(position)];
        let gap = rest.len() - rest.trim_start().len();
        if rest.trim_start().starts_with(')') {
            position += gap + 1;
        } else {
            return position;
        }
    }
}

/// Whether `stmt` already narrows `var` to named columns: `var = var[["a", "b"]]` or
/// `var = var.select("a", "b")`. Running the suggestion again on code it has already
/// changed must find nothing to do.
pub(crate) fn is_column_selection_of(stmt: &Stmt, var: &str) -> bool {
    let Stmt::Assign(assign) = stmt else {
        return false;
    };
    let [Expr::Name(target)] = assign.targets.as_slice() else {
        return false;
    };
    if target.id.as_str() != var {
        return false;
    }
    let is_var = |expr: &Expr| matches!(expr, Expr::Name(n) if n.id.as_str() == var);
    match &*assign.value {
        Expr::Subscript(subscript) => {
            is_var(&subscript.value)
                && matches!(&*subscript.slice, Expr::List(_))
                && crate::ast_extract::extract_string_list(&subscript.slice).is_some()
        }
        Expr::Call(call) => {
            matches!(&*call.func, Expr::Attribute(a) if a.attr.as_str() == "select" && is_var(&a.value))
                && !call.arguments.args.is_empty()
                && call.arguments.keywords.is_empty()
                && call
                    .arguments
                    .args
                    .iter()
                    .all(|a| matches!(a, Expr::StringLiteral(_)))
        }
        _ => false,
    }
}

// Pandas or polars. In order of how direct the evidence is: the variable's own annotation;
// the method that produced the frame (`.to_df()`, `.fetch_pandas_all()`, `.toPandas()` are
// pandas, `.pl()` polars); the module or imported name the expression starts from
// (`pd.read_parquet(...)`, `read_csv(...)` imported from pandas, connectorx's
// `return_type=`); the return annotation of a function it calls.
fn library_of(origin: &Origin) -> Result<Library, String> {
    let context = origin.libraries;
    if let Some(library) = origin
        .annotation
        .and_then(|a| context.annotation_library(a))
    {
        return Ok(library);
    }
    let value = origin
        .value
        .ok_or_else(|| "it has no assigned value".to_string())?;
    if let Expr::Call(call) = value {
        if let Expr::Attribute(attribute) = &*call.func {
            match attribute.attr.as_str() {
                "to_df" | "fetch_pandas_all" | "toPandas" | "to_pandas" | "to_dataframe" | "df" => {
                    return Ok(Library::Pandas)
                }
                "pl" | "to_polars" => return Ok(Library::Polars),
                _ => {}
            }
        }
    }
    let mut current = value;
    let mut innermost_call: Option<&ast::ExprCall> = None;
    // The attribute applied directly to the root name: `make` in `self.make().copy()`.
    let mut root_attribute: Option<&str> = None;
    loop {
        match current {
            Expr::Call(call) => {
                innermost_call = Some(call);
                current = &call.func;
            }
            Expr::Attribute(attribute) => {
                root_attribute = Some(attribute.attr.as_str());
                current = &attribute.value;
            }
            Expr::Name(name) => {
                let id = name.id.as_str();
                if matches!(id, "self" | "cls") && innermost_call.is_some() {
                    if let Some(library) = root_attribute.and_then(|m| context.functions.get(m)) {
                        return Ok(*library);
                    }
                }
                if let (Some(method), Some(class), true) = (
                    root_attribute,
                    origin.receivers.get(id),
                    innermost_call.is_some(),
                ) {
                    if let Some(library) = context
                        .class_methods
                        .get(&(class.clone(), method.to_string()))
                    {
                        return Ok(*library);
                    }
                }
                match context.modules.get(id) {
                    Some(ModuleKind::Library(library)) => return Ok(*library),
                    Some(ModuleKind::Connectorx) => return Ok(connectorx_library(innermost_call)),
                    None => {}
                }
                if let Some(library) = context.names.get(id).or_else(|| context.functions.get(id)) {
                    return Ok(*library);
                }
                let evidence = root_attribute
                    .filter(|method| {
                        context
                            .class_methods
                            .keys()
                            .any(|(_, defined)| defined == method)
                    })
                    .map(|method| {
                        format!(
                            "; `{method}` is annotated to return a DataFrame in a class of this \
                             file, but `{id}` is not known to be an instance of that class (it \
                             must be a local built as `{id} = Class(...)` or a parameter annotated \
                             `Class`, and not rebound)"
                        )
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "can't tell whether it is pandas or polars: the value is built from `{id}`, \
                     not a direct `pd.`/`pl.` call, a call to a function annotated \
                     `-> pd.DataFrame` or `-> pl.DataFrame`, or a method like `.to_df()`{evidence}"
                ));
            }
            _ => return Err("its origin isn't a call the library can be read from".to_string()),
        }
    }
}

// connectorx returns pandas unless `return_type="polars"` is passed.
fn connectorx_library(call: Option<&ast::ExprCall>) -> Library {
    let polars =
        call.is_some_and(|call| {
            call.arguments.keywords.iter().any(|keyword| {
            keyword.arg.as_ref().is_some_and(|a| a.as_str() == "return_type")
                && matches!(&keyword.value, Expr::StringLiteral(s) if s.value.to_str() == "polars")
        })
        });
    if polars {
        Library::Polars
    } else {
        Library::Pandas
    }
}

struct InsertionPoint {
    offset: usize,
    indent: String,
    // The statement is the last line of a file with no trailing newline.
    leading_newline: bool,
    newline: &'static str,
}

// The start of the line after the origin statement, indented like it. Refused when other
// code shares the origin's lines (`a = 1; df = load()`), since a new line after it would
// not directly follow the load.
fn insertion_point(origin: &Origin) -> Result<InsertionPoint, String> {
    let source = origin.source;
    let line_start = source[..origin.stmt_start].rfind('\n').map_or(0, |i| i + 1);
    let indent = &source[line_start..origin.stmt_start];
    if !indent.chars().all(|c| c == ' ' || c == '\t') {
        return Err("it shares a line with other code".to_string());
    }
    let rest = &source[origin.stmt_end..];
    let line_len = rest.find('\n').unwrap_or(rest.len());
    let trailing = rest[..line_len].trim_start();
    if !trailing.is_empty() && !trailing.starts_with('#') {
        return Err("it shares a line with other code".to_string());
    }
    let (offset, leading_newline) = match rest.find('\n') {
        Some(i) => (origin.stmt_end + i + 1, false),
        None => (source.len(), true),
    };
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    Ok(InsertionPoint {
        offset,
        indent: indent.to_string(),
        leading_newline,
        newline: if leading_newline { "" } else { newline },
    })
}

/// A unified diff of `old` -> `new`, or an empty string when they are equal. Assumes
/// the change is one contiguous region (true of every edit made here), found by
/// trimming the common leading and trailing lines.
pub(crate) fn render_unified_diff(path: &str, old: &str, new: &str) -> String {
    const CONTEXT: usize = 3;
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    if prefix == old_lines.len() && prefix == new_lines.len() {
        return String::new();
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let before = prefix.min(CONTEXT);
    let after = suffix.min(CONTEXT);
    let start = prefix - before;
    let old_changed_end = old_lines.len() - suffix;
    let new_changed_end = new_lines.len() - suffix;
    let old_end = old_changed_end + after;
    let new_end = new_changed_end + after;

    let mut out = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -{},{} +{},{} @@\n",
        start + 1,
        old_end - start,
        start + 1,
        new_end - start
    );
    let mut push = |marker: char, line: &str| {
        out.push(marker);
        out.push_str(line);
        out.push('\n');
    };
    for line in &old_lines[start..prefix] {
        push(' ', line);
    }
    for line in &old_lines[prefix..old_changed_end] {
        push('-', line);
    }
    for line in &new_lines[prefix..new_changed_end] {
        push('+', line);
    }
    for line in &old_lines[old_changed_end..old_end] {
        push(' ', line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column_usage::scan_usage;
    use ruff_python_ast::Stmt;
    use ruff_python_parser::parse_module;

    // The imports every test source is assumed to have at the top.
    fn test_libraries() -> LibraryContext {
        let imports = parse_module("import pandas as pd\nimport polars as pl\n")
            .unwrap()
            .into_syntax();
        LibraryContext::new(&imports.body, &HashMap::new())
    }

    // Runs the rule on the first assignment of `source`, scanning the rest of the module.
    fn run(source: &str, function_local: bool) -> Result<Suggestion, String> {
        let module = parse_module(source).unwrap().into_syntax();
        let first = module
            .body
            .iter()
            .position(|stmt| matches!(stmt, Stmt::Assign(_)))
            .expect("source must contain an assignment");
        let Stmt::Assign(assign) = &module.body[first] else {
            unreachable!()
        };
        let Expr::Name(target) = &assign.targets[0] else {
            panic!("target must be a name");
        };
        let usage = scan_usage(target.id.as_str(), &module.body[first + 1..], &|offset| {
            source[..offset.to_usize()].matches('\n').count() + 1
        });
        let mut libraries = test_libraries();
        libraries.extend(&module.body);
        suggest_selection(
            &Origin {
                libraries: &libraries,
                annotation: None,
                source,
                display_path: "f.py",
                value: Some(&assign.value),
                var: target.id.as_str(),
                line: 1,
                col: 1,
                stmt_start: assign.range().start().to_usize(),
                stmt_end: assign.range().end().to_usize(),
                function_local,
                receivers: &HashMap::new(),
            },
            &usage,
        )
    }

    fn applied(source: &str, suggestion: &Suggestion) -> String {
        let edit = &suggestion.edits[0];
        format!(
            "{}{}{}",
            &source[..edit.start],
            edit.replacement,
            &source[edit.end..]
        )
    }

    #[test]
    fn test_should_select_columns_with_a_pandas_subscript() {
        let source = "df = pd.read_json(\"x.parquet\")\nprint(df[\"a\"], df[\"b\"])\n";
        let s = run(source, true).unwrap();

        assert_eq!(s.kind, "select");
        assert_eq!(s.tier, "safe");
        assert_eq!(s.columns, vec!["a", "b"]);
        assert_eq!(
            applied(source, &s),
            "df = pd.read_json(\"x.parquet\")\ndf = df[[\"a\", \"b\"]]\nprint(df[\"a\"], df[\"b\"])\n"
        );
        assert!(s.diff.contains("+df = df[[\"a\", \"b\"]]"));
    }

    #[test]
    fn test_should_select_columns_with_a_polars_select() {
        let source = "df = pl.read_json(\"x.parquet\")\nprint(df[\"a\"])\n";
        let s = run(source, true).unwrap();

        assert!(applied(source, &s).contains("df = df.select(\"a\")\n"));
    }

    #[test]
    fn test_should_follow_a_method_chain_back_to_the_module() {
        let source = "df = pl.scan_csv(\"x.csv\").collect()\nprint(df[\"a\"])\n";
        assert!(run(source, true).unwrap().diff.contains(".select(\"a\")"));
        let source = "df = pd.read_csv(\"x.csv\").dropna()\nprint(df[\"a\"])\n";
        assert!(run(source, true).unwrap().diff.contains("df[[\"a\"]]"));
    }

    #[test]
    fn test_should_dedupe_columns_in_first_read_order() {
        let source = "df = pd.read_json(\"x\")\nprint(df[\"b\"], df[\"a\"], df[\"b\"])\n";
        assert_eq!(run(source, true).unwrap().columns, vec!["b", "a"]);
    }

    #[test]
    fn test_should_keep_the_indentation_of_the_origin() {
        let source = "def f():\n    df = pd.read_json(\"x\")\n    print(df[\"a\"])\n";
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::FunctionDef(f) = &module.body[0] else {
            panic!()
        };
        let Stmt::Assign(assign) = &f.body[0] else {
            panic!()
        };
        let usage = scan_usage("df", &f.body[1..], &|_| 3);
        let libraries = test_libraries();
        let s = suggest_selection(
            &Origin {
                libraries: &libraries,
                annotation: None,
                source,
                display_path: "f.py",
                value: Some(&assign.value),
                var: "df",
                line: 2,
                col: 5,
                stmt_start: assign.range().start().to_usize(),
                stmt_end: assign.range().end().to_usize(),
                function_local: true,
                receivers: &HashMap::new(),
            },
            &usage,
        )
        .unwrap();
        assert_eq!(
            applied(source, &s),
            "def f():\n    df = pd.read_json(\"x\")\n    df = df[[\"a\"]]\n    print(df[\"a\"])\n"
        );
    }

    #[test]
    fn test_should_insert_after_a_multiline_origin_and_its_comment() {
        let source = "df = pd.read_json(\n    \"x\",\n)  # load\nprint(df[\"a\"])\n";
        let s = run(source, true).unwrap();
        assert_eq!(
            applied(source, &s),
            "df = pd.read_json(\n    \"x\",\n)  # load\ndf = df[[\"a\"]]\nprint(df[\"a\"])\n"
        );
    }

    #[test]
    fn test_should_add_a_newline_when_the_origin_is_the_last_line() {
        let source = "df = pd.read_json(\"x\")";
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::Assign(assign) = &module.body[0] else {
            panic!()
        };
        let usage = ColumnUsage {
            accesses: vec![crate::column_usage::ColumnAccess {
                column: "a".to_string(),
                conditional: false,
                line: 1,
                via: None,
            }],
            ..ColumnUsage::default()
        };
        let libraries = test_libraries();
        let s = suggest_selection(
            &Origin {
                libraries: &libraries,
                annotation: None,
                source,
                display_path: "f.py",
                value: Some(&assign.value),
                var: "df",
                line: 1,
                col: 1,
                stmt_start: 0,
                stmt_end: source.len(),
                function_local: true,
                receivers: &HashMap::new(),
            },
            &usage,
        )
        .unwrap();
        assert_eq!(
            applied(source, &s),
            "df = pd.read_json(\"x\")\ndf = df[[\"a\"]]"
        );
    }

    #[test]
    fn test_should_withhold_when_other_code_shares_the_line() {
        let source = "df = pd.read_json(\"x\"); b = 2\nprint(df[\"a\"])\n";
        let reason = run(source, true).unwrap_err();
        assert!(reason.contains("shares a line"), "{reason}");
    }

    #[test]
    fn test_should_add_usecols_to_a_pandas_read_csv() {
        let source = "df = pd.read_csv(\"x.csv\", sep=\";\")\nprint(df[\"a\"], df[\"b\"])\n";
        let suggestion = run(source, true).unwrap();
        assert_eq!(suggestion.kind, "usecols");
        assert_eq!(suggestion.tier, "safe");
        assert_eq!(
            applied(source, &suggestion),
            "df = pd.read_csv(\"x.csv\", sep=\";\", usecols=[\"a\", \"b\"])\nprint(df[\"a\"], df[\"b\"])\n"
        );
        assert!(suggestion.diff.contains("+df = pd.read_csv"));
    }

    #[test]
    fn test_should_add_columns_to_a_read_parquet_and_a_polars_read_csv() {
        for (source, expected) in [
            (
                "df = pd.read_parquet(\"x\")\nprint(df[\"a\"])\n",
                "df = pd.read_parquet(\"x\", columns=[\"a\"])\nprint(df[\"a\"])\n",
            ),
            (
                "df = pl.read_csv(\"x\")\nprint(df[\"a\"])\n",
                "df = pl.read_csv(\"x\", columns=[\"a\"])\nprint(df[\"a\"])\n",
            ),
            (
                "df = pl.read_parquet(\"x\")\nprint(df[\"a\"])\n",
                "df = pl.read_parquet(\"x\", columns=[\"a\"])\nprint(df[\"a\"])\n",
            ),
        ] {
            let suggestion = run(source, true).unwrap();
            assert_eq!(suggestion.kind, "columns", "{source}");
            assert_eq!(applied(source, &suggestion), expected);
        }
    }

    #[test]
    fn test_should_add_the_column_list_after_the_last_argument_of_a_multiline_call() {
        let source = "df = pd.read_csv(\n    \"x\",\n    sep=\";\",\n)\nprint(df[\"a\"])\n";
        let suggestion = run(source, true).unwrap();
        assert_eq!(
            applied(source, &suggestion),
            "df = pd.read_csv(\n    \"x\",\n    sep=\";\", usecols=[\"a\"],\n)\nprint(df[\"a\"])\n"
        );
        parse_module(&applied(source, &suggestion)).expect("the edited source still parses");
    }

    #[test]
    fn test_should_add_usecols_to_a_read_csv_imported_by_name() {
        let source = "from pandas import read_csv\ndf = read_csv(\"x\")\nprint(df[\"a\"])\n";
        let suggestion = run(source, true).unwrap();
        assert_eq!(suggestion.kind, "usecols");
    }

    #[test]
    fn test_should_select_after_the_load_when_the_call_has_other_options() {
        for source in [
            "df = pd.read_csv(\"x\", parse_dates=[\"a\"])\nprint(df[\"a\"])\n",
            "df = pd.read_csv(\"x\", names=[\"a\", \"b\"])\nprint(df[\"a\"])\n",
            "df = pd.read_csv(\"x\", usecols=lambda c: True)\nprint(df[\"a\"])\n",
            "df = pd.read_csv(\"x\", **options)\nprint(df[\"a\"])\n",
            "df = pd.read_csv(*paths)\nprint(df[\"a\"])\n",
            "df = pd.read_csv(\"x\", \"y\")\nprint(df[\"a\"])\n",
            "df = pd.read_csv(\"x\").copy()\nprint(df[\"a\"])\n",
        ] {
            let suggestion = run(source, true).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(suggestion.kind, "select", "{source}");
        }
    }

    #[test]
    fn test_should_add_the_column_list_outside_a_parenthesised_argument() {
        for (source, expected) in [
            (
                "df = pd.read_csv((path))\nprint(df[\"a\"])\n",
                "df = pd.read_csv((path), usecols=[\"a\"])\nprint(df[\"a\"])\n",
            ),
            (
                "df = pd.read_csv(\"x\", sep=(\";\"))\nprint(df[\"a\"])\n",
                "df = pd.read_csv(\"x\", sep=(\";\"), usecols=[\"a\"])\nprint(df[\"a\"])\n",
            ),
            (
                "df = pd.read_csv(((path)) )\nprint(df[\"a\"])\n",
                "df = pd.read_csv(((path)), usecols=[\"a\"] )\nprint(df[\"a\"])\n",
            ),
        ] {
            let suggestion = run(source, true).unwrap();
            let edited = applied(source, &suggestion);
            assert_eq!(edited, expected);
            parse_module(&edited).expect("the edited source still parses");
        }
    }

    #[test]
    fn test_should_name_the_root_of_a_value_whose_library_is_unknown() {
        let reason = run("df = self.build()\nprint(df[\"a\"])\n", true).unwrap_err();
        assert!(reason.contains("built from `self`"), "{reason}");
    }

    #[test]
    fn test_should_withhold_when_the_library_is_unknown() {
        for source in [
            "df = read_csv(\"x\")\nprint(df[\"a\"])\n",
            "df = load()\nprint(df[\"a\"])\n",
            "df = [1]\nprint(df[\"a\"])\n",
        ] {
            assert!(run(source, true).is_err(), "{source}");
        }
        let reason = run("df = load()\nprint(df[\"a\"])\n", true).unwrap_err();
        assert!(reason.contains("pandas or polars"), "{reason}");
    }

    #[test]
    fn test_should_withhold_when_the_frame_is_passed_to_a_call() {
        let source = "df = pd.read_json(\"x\")\nhelper(df)\nprint(df[\"a\"])\n";
        let reason = run(source, true).unwrap_err();
        assert!(reason.contains("can't see everything"), "{reason}");
        assert!(reason.contains("line 2"), "{reason}");
    }

    #[test]
    fn test_should_count_additional_blockers() {
        let source = "df = pd.read_json(\"x\")\nhelper(df)\ndf.groupby(\"a\")\nprint(df[\"a\"])\n";
        assert!(run(source, true).unwrap_err().ends_with("and 1 more"));
    }

    #[test]
    fn test_should_not_be_blocked_by_index_empty_or_len() {
        let source =
            "df = pd.read_json(\"x\")\nprint(df.index, df.empty, len(df))\nprint(df[\"a\"])\n";
        assert!(run(source, true).is_ok());
    }

    #[test]
    fn test_should_block_on_columns_because_narrowing_changes_it() {
        let source = "df = pd.read_json(\"x\")\nprint(df.columns)\nprint(df[\"a\"])\n";
        assert!(run(source, true).is_err());
    }

    #[test]
    fn test_should_withhold_when_nothing_is_read() {
        let reason = run("df = pd.read_json(\"x\")\nprint(1)\n", true).unwrap_err();
        assert!(reason.contains("no column is read"), "{reason}");
    }

    #[test]
    fn test_should_not_count_a_column_created_in_scope_as_a_source_column() {
        let source = "df = pd.read_json(\"x\")\ndf[\"total\"] = df[\"a\"] * 2\nprint(df[\"total\"], df[\"b\"])\n";
        let s = run(source, true).unwrap();
        assert_eq!(s.columns, vec!["a", "b"]);
    }

    #[test]
    fn test_should_still_count_a_column_written_only_conditionally() {
        let source =
            "df = pd.read_json(\"x\")\nif flag:\n    df[\"c\"] = 1\nprint(df[\"c\"], df[\"a\"])\n";
        assert_eq!(run(source, true).unwrap().columns, vec!["c", "a"]);
    }

    #[test]
    fn test_should_note_pandas_assignment_into_a_selection_for_review() {
        let source = "df = pd.read_json(\"x\")\ndf[\"t\"] = df[\"a\"]\nprint(df[\"a\"])\n";
        let s = run(source, true).unwrap();
        assert_eq!(s.tier, "review");
        assert!(s.notes[0].contains("SettingWithCopyWarning"));
        let polars = "df = pl.read_json(\"x\")\ndf[\"t\"] = df[\"a\"]\nprint(df[\"a\"])\n";
        assert_eq!(run(polars, true).unwrap().tier, "safe");
    }

    #[test]
    fn test_should_mark_a_conditional_only_column_as_review() {
        let source = "df = pd.read_json(\"x\")\nif flag:\n    print(df[\"b\"])\nprint(df[\"a\"])\n";
        let s = run(source, true).unwrap();
        assert_eq!(s.tier, "review");
        assert!(s.notes[0].contains("'b' is only read conditionally"));
    }

    #[test]
    fn test_should_not_flag_a_column_also_read_unconditionally() {
        let source = "df = pd.read_json(\"x\")\nif flag:\n    print(df[\"a\"])\nprint(df[\"a\"])\n";
        assert_eq!(run(source, true).unwrap().tier, "safe");
    }

    #[test]
    fn test_should_mark_a_module_level_variable_as_review() {
        let source = "df = pd.read_json(\"x\")\nprint(df[\"a\"])\n";
        let s = run(source, false).unwrap();
        assert_eq!(s.tier, "review");
        assert!(s.notes[0].contains("module-level"));
    }

    #[test]
    fn test_should_escape_a_column_name_that_needs_it() {
        let source = "df = pd.read_json(\"x\")\nprint(df[\"a\\\"b\"])\n";
        assert!(run(source, true).unwrap().diff.contains(r#"df[["a\"b"]]"#));
    }

    #[test]
    fn test_should_render_an_empty_diff_when_nothing_changed() {
        assert_eq!(render_unified_diff("f.py", "a\nb\n", "a\nb\n"), "");
    }

    #[test]
    fn test_should_render_a_unified_diff_with_context() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n";
        let new = "1\n2\n3\n4\nX\n5\n6\n7\n8\n";
        let diff = render_unified_diff("f.py", old, new);
        assert!(diff.starts_with("--- a/f.py\n+++ b/f.py\n@@"), "{diff}");
        assert!(diff.contains("+X\n"));
        assert!(diff.contains(" 4\n") && diff.contains(" 7\n"));
    }

    #[test]
    fn test_should_render_a_diff_at_the_start_and_end_of_a_file() {
        let at_start = render_unified_diff("f.py", "a\nb\n", "X\na\nb\n");
        assert!(
            at_start.contains("+X\n") && at_start.contains("@@ -1,2 +1,3 @@"),
            "{at_start}"
        );
        let at_end = render_unified_diff("f.py", "a\nb\n", "a\nb\nX\n");
        assert!(
            at_end.contains("+X\n") && at_end.contains("@@ -1,2 +1,3 @@"),
            "{at_end}"
        );
    }

    // ── which library a frame belongs to ──

    fn selection_of(source: &str) -> String {
        run(source, true).unwrap().edits[0].replacement.clone()
    }

    #[test]
    fn test_should_follow_an_import_alias_to_the_library() {
        let source = "import pandas as p\ndf = p.read_json(\"x\")\nprint(df[\"a\"])\n";
        assert_eq!(selection_of(source), "df = df[[\"a\"]]\n");
    }

    #[test]
    fn test_should_follow_a_name_imported_from_a_library() {
        let source = "from polars import read_json\ndf = read_json(\"x\")\nprint(df[\"a\"])\n";
        assert_eq!(selection_of(source), "df = df.select(\"a\")\n");
    }

    #[test]
    fn test_should_read_the_library_from_the_method_that_produced_the_frame() {
        for (call, expected) in [
            (
                "store.get_historical_features(df).to_df()",
                "df = df[[\"a\"]]\n",
            ),
            ("cursor.fetch_pandas_all()", "df = df[[\"a\"]]\n"),
            ("spark.sql(q).toPandas()", "df = df[[\"a\"]]\n"),
            ("duckdb.sql(q).df()", "df = df[[\"a\"]]\n"),
            ("duckdb.sql(q).pl()", "df = df.select(\"a\")\n"),
            ("table.to_polars()", "df = df.select(\"a\")\n"),
        ] {
            let source = format!("df = {call}\nprint(df[\"a\"])\n");
            assert_eq!(selection_of(&source), expected, "{call}");
        }
    }

    #[test]
    fn test_should_default_connectorx_to_pandas_and_honour_return_type() {
        let pandas = "import connectorx as cx\ndf = cx.read_sql(uri, q)\nprint(df[\"a\"])\n";
        let polars = "import connectorx as cx\ndf = cx.read_sql(uri, q, return_type=\"polars\")\nprint(df[\"a\"])\n";
        assert_eq!(selection_of(pandas), "df = df[[\"a\"]]\n");
        assert_eq!(selection_of(polars), "df = df.select(\"a\")\n");
    }

    #[test]
    fn test_should_read_the_library_from_a_functions_return_annotation() {
        for (annotation, expected) in [
            ("pd.DataFrame", "df = df[[\"a\"]]\n"),
            ("pl.LazyFrame", "df = df.select(\"a\")\n"),
            ("pl.DataFrame | None", "df = df.select(\"a\")\n"),
            ("Optional[pd.DataFrame]", "df = df[[\"a\"]]\n"),
        ] {
            let source = format!(
                "from typing import Optional\ndef load() -> {annotation}:\n    ...\ndf = load()\nprint(df[\"a\"])\n"
            );
            assert_eq!(selection_of(&source), expected, "{annotation}");
        }
    }

    #[test]
    fn test_should_withhold_a_function_with_no_or_conflicting_return_annotations() {
        let unannotated = "def load():\n    ...\ndf = load()\nprint(df[\"a\"])\n";
        assert!(run(unannotated, true)
            .unwrap_err()
            .contains("pandas or polars"));
        let conflicting = "def load() -> pd.DataFrame:\n    ...\n\n\nclass A:\n    def load(self) -> pl.DataFrame:\n        ...\n\n\ndf = load()\nprint(df[\"a\"])\n";
        assert!(run(conflicting, true).is_err());
    }

    #[test]
    fn test_should_withhold_a_frame_from_an_unknown_system() {
        let source = "df = spark.sql(q)\nprint(df[\"a\"])\n";
        let reason = run(source, true).unwrap_err();
        assert!(reason.contains("`spark`"), "{reason}");
    }

    #[test]
    fn test_should_prefer_the_variables_own_annotation() {
        let source = "df: pl.DataFrame = fetch()\nprint(df[\"a\"])\n";
        let module = parse_module(source).unwrap().into_syntax();
        let Stmt::AnnAssign(ann) = &module.body[0] else {
            panic!()
        };
        let usage = scan_usage("df", &module.body[1..], &|_| 2);
        let libraries = test_libraries();
        let s = suggest_selection(
            &Origin {
                libraries: &libraries,
                annotation: Some(&ann.annotation),
                source,
                display_path: "f.py",
                value: ann.value.as_deref(),
                var: "df",
                line: 1,
                col: 1,
                stmt_start: 0,
                stmt_end: source.lines().next().unwrap().len(),
                function_local: true,
                receivers: &HashMap::new(),
            },
            &usage,
        )
        .unwrap();
        assert!(s.diff.contains(".select(\"a\")"));
    }
}
