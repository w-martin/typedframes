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
use crate::constants::STDLIB_MODULES;
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

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
    /// In a notebook, the 1-indexed cell and the line within it; `line` stays the position in
    /// the notebook's concatenated source, which is what edits and selections are keyed on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<(usize, usize)>,
}

/// A site that got no suggestion, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Withheld {
    pub phrase: String,
    pub line: usize,
    pub col: usize,
    pub var: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<(usize, usize)>,
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
    /// Every column name any frame in the project is known to have, for spotting a misspelling.
    pub(crate) known_columns: &'a [String],
    /// The function or module body the origin is in, for the library of a local it is built from.
    pub(crate) scope_body: &'a [Stmt],
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
    // Methods whose return annotation says pandas or polars, by `(class, method)`: a call on
    // a receiver known to be an instance of the class. This file's classes, plus those it
    // imports from other project files (from the index).
    class_methods: HashMap<(String, String), Library>,
}

impl LibraryContext {
    /// `imported_functions`: libraries of functions this file imports from other files.
    /// `imported_class_methods`: libraries of the methods of classes it imports, by
    /// `(class, method)`.
    pub(crate) fn new(
        body: &[Stmt],
        imported_functions: &HashMap<String, Library>,
        imported_class_methods: &HashMap<(String, String), Library>,
    ) -> LibraryContext {
        let mut context = LibraryContext {
            functions: imported_functions.clone(),
            ..LibraryContext::default()
        };
        context.collect_imports(body);
        // A name defined twice with different libraries can't be trusted either way.
        let mut own: HashMap<String, Option<Library>> = HashMap::new();
        collect_function_returns(body, &context, &mut own);
        let mut own_classes = HashSet::new();
        let mut bases = Vec::new();
        collect_classes(body, &mut own_classes, &mut bases);
        let inherited =
            inherited_method_libraries(&bases, &own_classes, &own, imported_class_methods);
        for (name, library) in own.into_iter().chain(inherited) {
            match library {
                Some(library) => context.functions.insert(name, library),
                None => context.functions.remove(&name),
            };
        }
        context.class_methods = class_method_libraries(body, &context);
        for ((class, method), library) in imported_class_methods {
            if !own_classes.contains(class) {
                context
                    .class_methods
                    .insert((class.clone(), method.clone()), *library);
            }
        }
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

// The names of the classes `body` defines, and `(subclass, base)` pairs for each base named
// plainly.
fn collect_classes(body: &[Stmt], names: &mut HashSet<String>, bases: &mut Vec<(String, String)>) {
    for stmt in body {
        match stmt {
            Stmt::ClassDef(class) => {
                names.insert(class.name.to_string());
                for base in class.bases() {
                    if let Expr::Name(base) = base {
                        bases.push((class.name.to_string(), base.id.to_string()));
                    }
                }
                collect_classes(&class.body, names, bases);
            }
            Stmt::FunctionDef(function) => collect_classes(&function.body, names, bases),
            _ => {}
        }
    }
}

// What `self.<method>()` returns for methods a class of this file inherits from an imported
// base, keyed by bare name like this file's own methods. A name this file defines itself is
// left to that definition; `None` once two imported bases disagree.
fn inherited_method_libraries(
    bases: &[(String, String)],
    own_classes: &HashSet<String>,
    own: &HashMap<String, Option<Library>>,
    imported_class_methods: &HashMap<(String, String), Library>,
) -> HashMap<String, Option<Library>> {
    let mut inherited: HashMap<String, Option<Library>> = HashMap::new();
    for (_, base) in bases.iter().filter(|(_, base)| !own_classes.contains(base)) {
        for ((class, method), library) in imported_class_methods {
            if class != base || own.contains_key(method) {
                continue;
            }
            inherited
                .entry(method.clone())
                .and_modify(|existing| {
                    if *existing != Some(*library) {
                        *existing = None;
                    }
                })
                .or_insert(Some(*library));
        }
    }
    inherited
}

/// The library each function of `module` returns, by name -- what the index stores so
/// other files can resolve a call to it.
pub(crate) fn function_libraries(module: &ModModule) -> HashMap<String, Library> {
    LibraryContext::new(&module.body, &HashMap::new(), &HashMap::new()).functions
}

/// The library each method of each class of `module` returns, by `(class, method)` -- what
/// the index stores so other files can resolve a call on an instance of the class.
pub(crate) fn class_method_libraries_of(module: &ModModule) -> HashMap<(String, String), Library> {
    LibraryContext::new(&module.body, &HashMap::new(), &HashMap::new()).class_methods
}

const MAX_NAMED_BLOCKERS: usize = 4;

/// `Ok` with a suggestion, or `Err` with the reason none could be made.
pub(crate) fn suggest_selection(
    origin: &Origin,
    usage: &ColumnUsage,
) -> Result<Suggestion, String> {
    let library = library_of(origin)?;
    let in_call = in_call_selection(origin, library);
    let insertion = insertion_point(origin)?;

    let mut blockers: Vec<&OtherUse> = usage
        .other_uses
        .iter()
        .filter(|u| crate::column_usage::blocks_narrowing(u))
        .collect();
    // A use that touches no columns (`n = df.shape[0]`) is rarely what makes narrowing
    // unsafe; name one that does.
    blockers.sort_by_key(|u| u.consumes == crate::column_usage::Consumption::None);
    if !blockers.is_empty() {
        // Name each use up to a handful, so the reason can be acted on without a second run.
        let named = blockers
            .iter()
            .take(MAX_NAMED_BLOCKERS)
            .map(|u| {
                let callee = u
                    .callee
                    .map_or_else(String::new, |class| format!(", {class}"));
                format!("{}{callee} (line {})", u.description, u.line)
            })
            .collect::<Vec<_>>()
            .join("; ");
        let more = match blockers.len().saturating_sub(MAX_NAMED_BLOCKERS) {
            0 => String::new(),
            n => format!(", and {n} more"),
        };
        return Err(format!("can't see everything done with it: {named}{more}"));
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
    // A name read once, nowhere else, a couple of edits from another column the project knows,
    // is as likely a typo as a column: the selection would write it down as if it were real.
    for column in &columns {
        let reads = usage
            .accesses
            .iter()
            .filter(|a| &a.column == column)
            .count();
        let quoted_in_file = origin.source.matches(&format!("\"{column}\"")).count()
            + origin.source.matches(&format!("'{column}'")).count();
        if reads != 1 || quoted_in_file > 1 {
            continue;
        }
        let near = columns
            .iter()
            .map(String::as_str)
            .chain(origin.known_columns.iter().map(String::as_str))
            .find(|other| {
                other != column
                    && column.chars().count() >= 5
                    && other.chars().count() >= 5
                    && crate::typo::levenshtein(column, other) <= 2
                    && crate::typo::levenshtein(column, other) * 2
                        <= column.chars().count().max(other.chars().count())
            });
        if let Some(other) = near {
            notes.push(format!(
                "'{column}' is read once and nowhere else in this file, and is a couple of edits \
                 from the known column '{other}'; if it is a misspelling the selection would keep it"
            ));
        }
    }
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
        cell: None,
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
    if let Some(library) = origin
        .annotation
        .and_then(|a| origin.libraries.annotation_library(a))
    {
        return Ok(library);
    }
    let value = origin
        .value
        .ok_or_else(|| "it has no assigned value".to_string())?;
    library_of_value(origin, value, 0)
}

/// How many locals deep `library_of_value` follows `b = a.rename(..)`, `c = b[mask]`, ...
const MAX_LOCAL_DEPTH: usize = 8;

// Whether assigning to `target` rebinds `name`; storing into `name["col"]` or `name.attr`
// does not.
fn binds(target: &Expr, name: &str) -> bool {
    match target {
        Expr::Name(n) => n.id.as_str() == name,
        Expr::Tuple(t) => t.elts.iter().any(|e| binds(e, name)),
        Expr::List(l) => l.elts.iter().any(|e| binds(e, name)),
        Expr::Starred(s) => binds(&s.value, name),
        _ => false,
    }
}

// Every assignment to `name` in `body` that ends before `before`, not looking inside nested
// functions or classes: the annotation, when there is one, and the value.
fn earlier_assignments<'a>(
    body: &'a [Stmt],
    name: &str,
    before: usize,
) -> Vec<(Option<&'a Expr>, Option<&'a Expr>)> {
    struct Finder<'a, 'n> {
        name: &'n str,
        before: usize,
        found: Vec<(Option<&'a Expr>, Option<&'a Expr>)>,
    }
    impl<'a> ast::visitor::source_order::SourceOrderVisitor<'a> for Finder<'a, '_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if stmt.range().start().to_usize() >= self.before
                || matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_))
            {
                return;
            }
            let name = self.name;
            let is_name = |e: &Expr| matches!(e, Expr::Name(n) if n.id.as_str() == name);
            let mentions = |e: &Expr| binds(e, name);
            // Bound by a loop or `with` target: the value can't be read here.
            let loop_or_with = match stmt {
                Stmt::For(f) => mentions(&f.target),
                Stmt::With(w) => w
                    .items
                    .iter()
                    .any(|i| i.optional_vars.as_deref().is_some_and(mentions)),
                _ => false,
            };
            if loop_or_with {
                self.found.push((None, None));
            }
            if stmt.range().end().to_usize() > self.before {
                ast::visitor::source_order::walk_stmt(self, stmt);
                return;
            }
            match stmt {
                Stmt::Assign(a) if a.targets.iter().any(is_name) => {
                    self.found.push((None, Some(&a.value)));
                }
                Stmt::AnnAssign(a) if is_name(&a.target) => {
                    self.found.push((Some(&a.annotation), a.value.as_deref()));
                }
                // Unpacked or augmented: the value can't be read here.
                Stmt::Assign(a) if a.targets.iter().any(mentions) => self.found.push((None, None)),
                Stmt::AugAssign(a) if is_name(&a.target) => self.found.push((None, None)),
                _ => ast::visitor::source_order::walk_stmt(self, stmt),
            }
        }
        fn visit_expr(&mut self, _expr: &'a Expr) {}
    }
    let mut finder = Finder {
        name,
        before,
        found: Vec::new(),
    };
    ast::visitor::source_order::SourceOrderVisitor::visit_body(&mut finder, body);
    finder.found
}

// The library of `id`, a local of the origin's scope bound before `before`: the one every
// earlier assignment to it agrees on.
fn local_library(origin: &Origin, id: &str, before: usize, depth: usize) -> Option<Library> {
    if depth >= MAX_LOCAL_DEPTH {
        return None;
    }
    let mut agreed = None;
    for (annotation, value) in earlier_assignments(origin.scope_body, id, before) {
        let library = annotation
            .and_then(|a| origin.libraries.annotation_library(a))
            .or_else(|| value.and_then(|v| library_of_value(origin, v, depth + 1).ok()))?;
        if agreed.is_some_and(|known| known != library) {
            return None;
        }
        agreed = Some(library);
    }
    agreed
}

fn library_of_value(origin: &Origin, value: &Expr, depth: usize) -> Result<Library, String> {
    let context = origin.libraries;
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
    // The one before it, when there is one: `store` in `self.store.read()`.
    let mut attribute_before_root: Option<&str> = None;
    loop {
        match current {
            Expr::Call(call) => {
                innermost_call = Some(call);
                current = &call.func;
            }
            Expr::Attribute(attribute) => {
                attribute_before_root = root_attribute;
                root_attribute = Some(attribute.attr.as_str());
                current = &attribute.value;
            }
            Expr::Subscript(subscript) => current = &subscript.value,
            Expr::Name(name) => {
                let id = name.id.as_str();
                // `self.store.read()`: `store` holds an instance of a class this file names.
                if let (true, Some(method), Some(attr), true) = (
                    id == "self",
                    attribute_before_root,
                    root_attribute,
                    innermost_call.is_some(),
                ) {
                    if let Some(library) =
                        origin
                            .receivers
                            .get(&format!("self.{attr}"))
                            .and_then(|class| {
                                context
                                    .class_methods
                                    .get(&(class.clone(), method.to_string()))
                            })
                    {
                        return Ok(*library);
                    }
                }
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
                if let Some(library) =
                    local_library(origin, id, value.range().start().to_usize(), depth)
                {
                    return Ok(library);
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
/// Where `Final` comes from in a module: a name already imported, or the line to add.
enum FinalName {
    Imported(String),
    Add(Edit),
}

// The line ending the file uses where `offset` is: `\r\n` if the line before it ends that way.
fn line_ending_at(source: &str, offset: usize) -> &'static str {
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |i| offset + i);
    if source[..line_end].ends_with('\r') {
        "\r\n"
    } else {
        "\n"
    }
}

// The end of the logical line holding the offset `end`: past a trailing comment, a `;` and
// whatever follows it, and past a statement that begins on this line and continues onto the
// next. A new line inserted here is never inside another statement or between code and
// its comment.
fn logical_line_end(module: &ModModule, source: &str, end: usize) -> usize {
    let mut at = end;
    loop {
        let newline = source[at..].find('\n').map_or(source.len(), |i| at + i);
        let continued = module.body.iter().find(|stmt| {
            let range = stmt.range();
            (at..newline).contains(&range.start().to_usize()) && range.end().to_usize() > newline
        });
        match continued {
            Some(stmt) => at = stmt.range().end().to_usize(),
            None => return newline,
        }
    }
}

fn rebinds_final(module: &ModModule) -> bool {
    module.body.iter().any(|stmt| match stmt {
        Stmt::ClassDef(c) => c.name.as_str() == "Final",
        Stmt::FunctionDef(f) => f.name.as_str() == "Final",
        Stmt::ImportFrom(import) => {
            let typing = import
                .module
                .as_ref()
                .is_some_and(|m| matches!(m.as_str(), "typing" | "typing_extensions"));
            !typing
                && import
                    .names
                    .iter()
                    .any(|a| a.asname.as_ref().unwrap_or(&a.name).as_str() == "Final")
        }
        other => binds_name(other, "Final"),
    })
}

// Adds `Final` to an existing top-level `from typing import ...` instead of a second import
// line, which an import sorter (ruff's `I001`) would then reject. Placed among the names
// the way isort orders them: CamelCase names alphabetically, lowercase ones after.
// Ruff's isort order for imported members (`order-by-type`, case-insensitive): constants
// (`TYPE_CHECKING`), then classes (`Any`), then everything else, each alphabetical.
fn isort_member_key(name: &str) -> (u8, String) {
    let constant = name.chars().count() > 1
        && name.chars().any(char::is_alphabetic)
        && !name.chars().any(char::is_lowercase);
    let rank = if constant {
        0
    } else if name.chars().next().is_some_and(char::is_uppercase) {
        1
    } else {
        2
    };
    (rank, name.to_lowercase())
}

fn merge_into_typing_import(module: &ModModule) -> Option<Edit> {
    let import = module.body.iter().find_map(|stmt| match stmt {
        Stmt::ImportFrom(import)
            if import.level == 0
                && import
                    .module
                    .as_ref()
                    .is_some_and(|m| m.as_str() == "typing")
                && import.names.iter().all(|a| a.name.as_str() != "*") =>
        {
            Some(import)
        }
        _ => None,
    })?;
    let final_key = isort_member_key("Final");
    let before = import
        .names
        .iter()
        .find(|a| isort_member_key(a.name.as_str()) > final_key);
    let (at, replacement) = match before {
        Some(alias) => (alias.range().start().to_usize(), "Final, "),
        None => (import.names.last()?.range().end().to_usize(), ", Final"),
    };
    Some(Edit {
        start: at,
        end: at,
        replacement: replacement.to_string(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ImportSection {
    Future,
    Stdlib,
    Other,
}

fn import_module(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Import(import) => import
            .names
            .first()
            .map_or(String::new(), |a| a.name.to_string()),
        Stmt::ImportFrom(import) => {
            let dots = ".".repeat(import.level as usize);
            format!(
                "{dots}{}",
                import.module.as_ref().map_or("", |m| m.as_str())
            )
        }
        _ => String::new(),
    }
}

fn import_section(stmt: &Stmt) -> ImportSection {
    let module = import_module(stmt);
    let top = module.split('.').next().unwrap_or("");
    if matches!(stmt, Stmt::ImportFrom(i) if i.level == 0) && top == "__future__" {
        ImportSection::Future
    } else if !module.starts_with('.') && STDLIB_MODULES.contains(&top) {
        ImportSection::Stdlib
    } else {
        ImportSection::Other
    }
}

// How an import sorter orders statements within a section: `import x` before `from x import`,
// then by module name without regard to case.
fn import_key(stmt: &Stmt) -> (u8, String) {
    (
        u8::from(matches!(stmt, Stmt::ImportFrom(_))),
        import_module(stmt).to_lowercase(),
    )
}

// The import statements opening the file, after any docstring.
fn leading_imports(module: &ModModule) -> Vec<&Stmt> {
    let is_docstring =
        |stmt: &Stmt| matches!(stmt, Stmt::Expr(e) if matches!(&*e.value, Expr::StringLiteral(_)));
    module
        .body
        .iter()
        .skip_while(|stmt| is_docstring(stmt))
        .take_while(|stmt| matches!(stmt, Stmt::Import(_) | Stmt::ImportFrom(_)))
        .collect()
}

/// Whether the file's opening imports are grouped and ordered the way an import sorter
/// (ruff's `I001`) wants them, as far as sections and the standard library go: `__future__`,
/// then the standard library, then everything else, a blank line between groups and none
/// within one.
pub(crate) fn imports_in_order(source: &str) -> bool {
    let Ok(module) = ruff_python_parser::parse_module(source) else {
        return true;
    };
    let module = module.into_syntax();
    let leading = leading_imports(&module);
    leading.windows(2).all(|pair| {
        let (before, after) = (import_section(pair[0]), import_section(pair[1]));
        let between = &source[pair[0].range().end().to_usize()..pair[1].range().start().to_usize()];
        let blank_lines = between.matches('\n').count().saturating_sub(1);
        match before.cmp(&after) {
            std::cmp::Ordering::Less => blank_lines >= 1,
            std::cmp::Ordering::Equal => match after {
                // Third-party and first-party code are groups of their own.
                ImportSection::Other => blank_lines <= 1,
                _ => blank_lines == 0 && import_key(pair[0]) <= import_key(pair[1]),
            },
            std::cmp::Ordering::Greater => false,
        }
    })
}

fn final_name(module: &ModModule, source: &str) -> Result<FinalName, String> {
    if rebinds_final(module) {
        return Err("`Final` is already bound to something else in this file".to_string());
    }
    for stmt in &module.body {
        match stmt {
            Stmt::ImportFrom(import)
                if import.level == 0
                    && import
                        .module
                        .as_ref()
                        .is_some_and(|m| matches!(m.as_str(), "typing" | "typing_extensions")) =>
            {
                if let Some(alias) = import.names.iter().find(|a| a.name.as_str() == "Final") {
                    let name = alias.asname.as_ref().unwrap_or(&alias.name);
                    return Ok(FinalName::Imported(name.to_string()));
                }
            }
            Stmt::Import(import) => {
                if let Some(alias) = import.names.iter().find(|a| a.name.as_str() == "typing") {
                    let name = alias.asname.as_ref().map_or("typing", |a| a.as_str());
                    return Ok(FinalName::Imported(format!("{name}.Final")));
                }
            }
            _ => {}
        }
    }
    if let Some(edit) = merge_into_typing_import(module) {
        return Ok(FinalName::Add(edit));
    }
    let is_docstring =
        |stmt: &Stmt| matches!(stmt, Stmt::Expr(e) if matches!(&*e.value, Expr::StringLiteral(_)));
    let leading = leading_imports(module);
    let line_start = |stmt: &Stmt| {
        let start = stmt.range().start().to_usize();
        source[..start].rfind('\n').map_or(0, |i| i + 1)
    };
    // Where an import sorter (ruff's `I001`) wants `from typing import Final`: inside the
    // standard-library group, by module name, or in a group of its own ahead of the rest.
    let stdlib: Vec<&Stmt> = leading
        .iter()
        .copied()
        .filter(|stmt| import_section(stmt) == ImportSection::Stdlib)
        .collect();
    let target = (1, "typing".to_string());
    let after_last = |last: &Stmt| {
        // Inserted before the line's newline, so in a notebook the import stays in
        // the cell that holds the last import.
        let at = logical_line_end(module, source, last.range().end().to_usize());
        let eol = line_ending_at(source, at.saturating_sub(1));
        let at = if source[..at].ends_with('\r') {
            at - 1
        } else {
            at
        };
        (at, eol)
    };
    let edit = if let Some(next) = stdlib.iter().find(|stmt| import_key(stmt) > target) {
        let at = line_start(next);
        let eol = line_ending_at(source, at.saturating_sub(1));
        Edit {
            start: at,
            end: at,
            replacement: format!("from typing import Final{eol}"),
        }
    } else if let Some(last) = stdlib.last() {
        let (at, eol) = after_last(last);
        Edit {
            start: at,
            end: at,
            replacement: format!("{eol}from typing import Final"),
        }
    } else if let Some(first_other) = leading
        .iter()
        .find(|stmt| import_section(stmt) != ImportSection::Future)
    {
        let at = line_start(first_other);
        let eol = line_ending_at(source, at.saturating_sub(1));
        Edit {
            start: at,
            end: at,
            replacement: format!("from typing import Final{eol}{eol}"),
        }
    } else if let Some(last) = leading.last() {
        let (at, eol) = after_last(last);
        Edit {
            start: at,
            end: at,
            replacement: format!("{eol}{eol}from typing import Final"),
        }
    } else {
        let first = module.body.iter().find(|stmt| !is_docstring(stmt));
        let at = first.map_or(source.len(), line_start);
        let eol = line_ending_at(source, at.saturating_sub(1));
        Edit {
            start: at,
            end: at,
            replacement: format!("from typing import Final{eol}{eol}"),
        }
    };
    Ok(FinalName::Add(edit))
}

/// The one top-level class named `name`, if the file defines exactly one.
fn local_class<'a>(module: &'a ModModule, name: &str) -> Option<&'a ast::StmtClassDef> {
    let mut found = module.body.iter().filter_map(|stmt| match stmt {
        Stmt::ClassDef(c) if c.name.as_str() == name => Some(c),
        _ => None,
    });
    match (found.next(), found.next()) {
        (Some(class), None) => Some(class),
        _ => None,
    }
}

fn is_class_var(expr: &Expr) -> bool {
    match expr {
        Expr::Name(name) => name.id.as_str() == "ClassVar",
        Expr::Attribute(attr) => attr.attr.as_str() == "ClassVar",
        _ => false,
    }
}

fn binds_name(stmt: &Stmt, attr: &str) -> bool {
    let is_attr = |target: &Expr| matches!(target, Expr::Name(n) if n.id.as_str() == attr);
    match stmt {
        Stmt::Assign(a) => a.targets.iter().any(is_attr),
        Stmt::AnnAssign(a) => is_attr(&a.target),
        _ => false,
    }
}

/// The name of the class in this file that `attr` resolves to when read through `class`,
/// and its one binding. Bases are searched in order; a base this file does not define
/// might supply `attr` first, so reaching one ends the search.
fn defining_binding<'a>(
    module: &'a ModModule,
    class: &str,
    attr: &str,
    visiting: &mut Vec<String>,
) -> Result<(&'a str, &'a Stmt), String> {
    let Some(class_def) = local_class(module, class) else {
        return Err(format!(
            "class `{class}` is not defined exactly once at the top level of this file"
        ));
    };
    if visiting.iter().any(|seen| seen == class) {
        return Err(format!("`{class}` inherits from itself"));
    }
    visiting.push(class.to_string());
    let mut own = class_def.body.iter().filter(|stmt| binds_name(stmt, attr));
    match (own.next(), own.next()) {
        (Some(binding), None) => return Ok((class_def.name.as_str(), binding)),
        (Some(_), Some(_)) => {
            return Err(format!("`{attr}` is assigned more than once in `{class}`"))
        }
        _ => {}
    }
    for base in class_def.bases() {
        match base {
            Expr::Name(name) if local_class(module, name.id.as_str()).is_some() => {
                if let Ok(found) = defining_binding(module, name.id.as_str(), attr, visiting) {
                    return Ok(found);
                }
            }
            _ => {
                return Err(format!(
                    "`{attr}` is not assigned in `{class}`; it may come from a base class \
                     defined in another file"
                ))
            }
        }
    }
    Err(format!(
        "`{attr}` is not assigned in `{class}` or its bases in this file"
    ))
}

fn descends_from(module: &ModModule, class: &str, ancestor: &str, depth: usize) -> bool {
    let Some(class_def) = local_class(module, class) else {
        return false;
    };
    depth < 32
        && class_def.bases().iter().any(|base| match base {
            Expr::Name(name) => {
                name.id.as_str() == ancestor
                    || descends_from(module, name.id.as_str(), ancestor, depth + 1)
            }
            _ => false,
        })
}

/// A class in this file, below `defining`, that assigns `attr` again.
fn rebinding_subclass<'a>(module: &'a ModModule, defining: &str, attr: &str) -> Option<&'a str> {
    module.body.iter().find_map(|stmt| match stmt {
        Stmt::ClassDef(c)
            if c.name.as_str() != defining
                && c.body.iter().any(|s| binds_name(s, attr))
                && descends_from(module, c.name.as_str(), defining, 0) =>
        {
            Some(c.name.as_str())
        }
        _ => None,
    })
}

/// Whether `name` appears in `source` as an identifier outside the byte ranges `except`.
fn mentions_outside(source: &str, name: &str, except: &[std::ops::Range<usize>]) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    source.match_indices(name).any(|(at, _)| {
        let end = at + name.len();
        !except.iter().any(|r| r.start <= at && end <= r.end)
            && !source[..at].chars().next_back().is_some_and(is_ident)
            && !source[end..].chars().next().is_some_and(is_ident)
    })
}

/// The edit that drops `ClassVar` from `from typing import ...` once the rewrite has removed
/// its last use, so the fix does not leave an unused import (`F401`) behind. It reuses the
/// `, Final` insertion in `edits` when `ClassVar` was the only name imported.
fn drop_orphaned_class_var(
    module: &ModModule,
    source: &str,
    edits: &mut Vec<Edit>,
    replaced: std::ops::Range<usize>,
) {
    let Some(import) = module.body.iter().find_map(|stmt| match stmt {
        Stmt::ImportFrom(import)
            if import.level == 0
                && import
                    .module
                    .as_ref()
                    .is_some_and(|m| m.as_str() == "typing")
                && import.names.iter().all(|a| a.name.as_str() != "*") =>
        {
            Some(import)
        }
        _ => None,
    }) else {
        return;
    };
    let Some(position) = import
        .names
        .iter()
        .position(|a| a.name.as_str() == "ClassVar" && a.asname.is_none())
    else {
        return;
    };
    let import_range = import.range().start().to_usize()..import.range().end().to_usize();
    if mentions_outside(source, "ClassVar", &[import_range, replaced]) {
        return;
    }
    let alias = &import.names[position];
    let (start, end) = (
        alias.range().start().to_usize(),
        alias.range().end().to_usize(),
    );
    if import.names.len() == 1 {
        if let Some(insertion) = edits
            .iter_mut()
            .find(|e| e.start == end && e.end == end && e.replacement == ", Final")
        {
            *insertion = Edit {
                start,
                end,
                replacement: "Final".to_string(),
            };
        }
        return;
    }
    let edit = match import.names.get(position + 1) {
        Some(next) => Edit {
            start,
            end: next.range().start().to_usize(),
            replacement: String::new(),
        },
        None => Edit {
            start: import.names[position - 1].range().end().to_usize(),
            end,
            replacement: String::new(),
        },
    };
    edits.push(edit);
}

/// `source` without a `ClassVar` import that nothing uses any more, as left behind when
/// several constants of one file were each rewritten from `ClassVar` to `Final`.
pub(crate) fn without_orphaned_class_var(original: &str, source: &str) -> String {
    // An import that was already unused is not ours to remove.
    let line_of_import = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("from typing import") && l.contains("ClassVar"))
            .map(str::to_string)
    };
    let used_before = line_of_import(original).is_some_and(|import| {
        let rest = original.replacen(&import, "", 1);
        mentions_outside(&rest, "ClassVar", &[])
    });
    if !used_before {
        return source.to_string();
    }
    let Ok(module) = ruff_python_parser::parse_module(source) else {
        return source.to_string();
    };
    let module = module.into_syntax();
    let mut edits = Vec::new();
    drop_orphaned_class_var(&module, source, &mut edits, 0..0);
    // Only a plain removal: the `, Final` swap belongs to the single-constant path.
    edits.retain(|e| e.replacement.is_empty());
    crate::apply::apply_edits(source, &edits).unwrap_or_else(|| source.to_string())
}

/// `NAME: Final = ...` for a class constant read through `self`, so no subclass in another
/// file can rebind what the checker resolved. Withheld unless the constant is one plain
/// assignment directly in the body of a top-level class of this file, the reading class or
/// a base of it, and no class below that one assigns it again.
pub(crate) fn suggest_final_constant(
    module: &ModModule,
    source: &str,
    display_path: &str,
    class: &str,
    attr: &str,
) -> Result<Suggestion, String> {
    let (defining, binding) = defining_binding(module, class, attr, &mut Vec::new())?;
    if let Some(rebinder) = rebinding_subclass(module, defining, attr) {
        return Err(format!(
            "`{rebinder}` rebinds `{attr}`, which `Final` would forbid"
        ));
    }
    let class = defining;

    let (target, mut edits) = match binding {
        Stmt::Assign(assign) => {
            let [target] = assign.targets.as_slice() else {
                return Err(format!("`{attr}` is assigned in a chained assignment"));
            };
            (target, Vec::new())
        }
        Stmt::AnnAssign(ann) => {
            if ann.value.is_none() {
                return Err(format!("`{attr}` is annotated but not assigned a value"));
            }
            (&*ann.target, Vec::new())
        }
        _ => unreachable!("filtered to assignments above"),
    };
    let final_ref = match final_name(module, source)? {
        FinalName::Imported(name) => name,
        FinalName::Add(edit) => {
            edits.push(edit);
            "Final".to_string()
        }
    };
    match binding {
        Stmt::AnnAssign(ann) => {
            let range = ann.annotation.range();
            // `Final` already means a class-level constant, and `Final[ClassVar[T]]` is invalid.
            let replacement = match &*ann.annotation {
                Expr::Subscript(sub) if is_class_var(&sub.value) => {
                    format!("{final_ref}[{}]", &source[sub.slice.range()])
                }
                bare if is_class_var(bare) => final_ref.clone(),
                _ => format!("{final_ref}[{}]", &source[range]),
            };
            edits.push(Edit {
                start: range.start().to_usize(),
                end: range.end().to_usize(),
                replacement,
            });
            drop_orphaned_class_var(
                module,
                source,
                &mut edits,
                range.start().to_usize()..range.end().to_usize(),
            );
        }
        _ => {
            let end = target.range().end().to_usize();
            edits.push(Edit {
                start: end,
                end,
                replacement: format!(": {final_ref}"),
            });
        }
    }

    let edited = crate::apply::apply_edits(source, &edits)
        .ok_or_else(|| "its edits overlap or don't fit the file".to_string())?;
    let line = source[..target.range().start().to_usize()]
        .matches('\n')
        .count()
        + 1;
    let col = target.range().start().to_usize()
        - source[..target.range().start().to_usize()]
            .rfind('\n')
            .map_or(0, |i| i + 1)
        + 1;
    Ok(Suggestion {
        phrase: "assumed-constant".to_string(),
        kind: "final",
        line,
        col,
        var: attr.to_string(),
        tier: "safe",
        columns: Vec::new(),
        reason: format!(
            "`{attr}` of `{class}` is read through `self` as a constant; declaring it `Final` \
             rules out a subclass in another file rebinding it (a type checker will flag one)"
        ),
        notes: Vec::new(),
        diff: render_unified_diff(display_path, source, &edited),
        edits,
        cell: None,
    })
}

// The lines between the trimmed ends, in order. Edits in two places leave the code between
// them in both versions; shown as context rather than removed and added again. Past a size
// where the table gets large, all removals then all additions.
fn interleave<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    const MAX_CELLS: usize = 4_000_000;
    let (n, m) = (old.len(), new.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_CELLS {
        let removed = old.iter().map(|l| ('-', *l));
        return removed.chain(new.iter().map(|l| ('+', *l))).collect();
    }
    // shared[i][j]: the longest run of lines old[i..] and new[j..] have in common.
    let mut shared = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            shared[i][j] = if old[i] == new[j] {
                shared[i + 1][j + 1] + 1
            } else {
                shared[i + 1][j].max(shared[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(n + m);
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            out.push((' ', old[i]));
            i += 1;
            j += 1;
        } else if j == m || (i < n && shared[i + 1][j] >= shared[i][j + 1]) {
            out.push(('-', old[i]));
            i += 1;
        } else {
            out.push(('+', new[j]));
            j += 1;
        }
    }
    out
}

// A shortest edit script between `old` and `new` (Myers), or `None` once it takes more than
// `MAX_EDITS` insertions and deletions, where the table would get large.
fn shortest_edit_script<'a>(old: &[&'a str], new: &[&'a str]) -> Option<Vec<(char, &'a str)>> {
    const MAX_EDITS: isize = 1000;
    let (n, m) = (old.len() as isize, new.len() as isize);
    let limit = (n + m).min(MAX_EDITS);
    let offset = limit + 1;
    let at = |k: isize| (k + offset) as usize;
    let mut v = vec![0isize; (2 * limit + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=limit {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let last = found?;
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=last).rev() {
        let before = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && before[at(k - 1)] < before[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let (prev_x, prev_y) = (before[at(prev_k)], before[at(prev_k)] - prev_k);
        while x > prev_x && y > prev_y {
            ops.push((' ', old[(x - 1) as usize]));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                ops.push(('+', new[(y - 1) as usize]));
            } else {
                ops.push(('-', old[(x - 1) as usize]));
            }
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    Some(ops)
}

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
    let old_changed_end = old_lines.len() - suffix;
    let new_changed_end = new_lines.len() - suffix;
    let middle_old = &old_lines[prefix..old_changed_end];
    let middle_new = &new_lines[prefix..new_changed_end];
    let middle = shortest_edit_script(middle_old, middle_new)
        .unwrap_or_else(|| interleave(middle_old, middle_new));

    let mut ops: Vec<(char, &str)> = old_lines[..prefix].iter().map(|l| (' ', *l)).collect();
    ops.extend(middle);
    ops.extend(old_lines[old_changed_end..].iter().map(|l| (' ', *l)));

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (marker, _))| *marker != ' ')
        .map(|(i, _)| i)
        .collect();
    // Changes closer together than two contexts share a hunk.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        let from = i.saturating_sub(CONTEXT);
        let to = (i + CONTEXT + 1).min(ops.len());
        match hunks.last_mut() {
            Some(last) if from <= last.1 => last.1 = to,
            _ => hunks.push((from, to)),
        }
    }

    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    for (from, to) in hunks {
        let old_start = ops[..from].iter().filter(|(m, _)| *m != '+').count() + 1;
        let new_start = ops[..from].iter().filter(|(m, _)| *m != '-').count() + 1;
        let old_len = ops[from..to].iter().filter(|(m, _)| *m != '+').count();
        let new_len = ops[from..to].iter().filter(|(m, _)| *m != '-').count();
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for (marker, line) in &ops[from..to] {
            out.push(*marker);
            out.push_str(line);
            out.push('\n');
        }
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
        LibraryContext::new(&imports.body, &HashMap::new(), &HashMap::new())
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
                known_columns: &[],
                scope_body: &module.body,
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
                known_columns: &[],
                scope_body: &[],
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
                known_columns: &[],
                scope_body: &[],
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
    fn test_should_suggest_when_shape_and_head_are_only_displayed() {
        // arrange: nothing but display uses besides the column reads
        let source = "df = pd.read_csv(\"x.csv\")\nprint(df.shape)\nprint(df.head())\ndf.head()\nprint(df[\"a\"])\n";

        // act
        let suggestion = run(source, true).unwrap();

        // assert: narrowing goes ahead, with the display flagged for review
        assert!(
            suggestion.notes.iter().any(|n| n.contains("displayed")),
            "{:?}",
            suggestion.notes
        );
    }

    #[test]
    fn test_should_name_the_use_that_blocks_not_a_shape_read_first() {
        // arrange
        let source = "df = pd.read_csv(\"x.csv\")\nn = df.shape[0]\nhelper(df)\nprint(df[\"a\"])\n";

        // act
        let reason = run(source, true).unwrap_err();

        // assert
        assert!(reason.contains("passed to"), "{reason}");
        assert!(reason.contains("line 3"), "{reason}");
    }

    #[test]
    fn test_should_withhold_when_the_frame_is_passed_to_a_call() {
        let source = "df = pd.read_json(\"x\")\nhelper(df)\nprint(df[\"a\"])\n";
        let reason = run(source, true).unwrap_err();
        assert!(reason.contains("can't see everything"), "{reason}");
        assert!(reason.contains("line 2"), "{reason}");
    }

    #[test]
    fn test_should_name_every_blocker_up_to_a_handful_then_count_the_rest() {
        let few = "df = pd.read_json(\"x\")\nhelper(df)\ndf.groupby(\"a\")\nprint(df[\"a\"])\n";
        let many = "df = pd.read_json(\"x\")\nh1(df)\nh2(df)\nh3(df)\nh4(df)\nh5(df)\nh6(df)\nprint(df[\"a\"])\n";

        let reason = run(few, true).unwrap_err();
        let overflow = run(many, true).unwrap_err();

        assert!(
            reason.contains("(line 2)") && reason.contains("(line 3)"),
            "{reason}"
        );
        assert!(!reason.contains("more"), "{reason}");
        assert!(overflow.ends_with(", and 2 more"), "{overflow}");
    }

    #[test]
    fn test_should_not_be_blocked_by_index_empty_or_len() {
        let source =
            "df = pd.read_json(\"x\")\nprint(df.index, df.empty, len(df))\nprint(df[\"a\"])\n";
        assert!(run(source, true).is_ok());
    }

    #[test]
    fn test_should_block_on_columns_because_narrowing_changes_it() {
        let source = "df = pd.read_json(\"x\")\nkeep = [c for c in df.columns]\nprint(df[\"a\"])\n";
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
    fn test_should_mark_a_column_read_once_next_to_a_near_miss_as_review() {
        // arrange: `risk_calc` is read once and nowhere else, two edits from `risk_cal`
        let source = "df = pd.read_json(\"x\")\nprint(df[\"risk_cal\"], df[\"risk_cal\"], df[\"risk_calc\"])\n";

        // act
        let s = run(source, true).unwrap();

        // assert
        assert_eq!(s.tier, "review");
        assert!(s
            .notes
            .iter()
            .any(|n| n.contains("'risk_calc'") && n.contains("misspelling")));
    }

    #[test]
    fn test_should_keep_distinct_columns_read_once_each_safe() {
        let source = "df = pd.read_json(\"x\")\nprint(df[\"age\"], df[\"income\"])\n";
        assert_eq!(run(source, true).unwrap().tier, "safe");
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
                known_columns: &[],
                scope_body: &[],
            },
            &usage,
        )
        .unwrap();
        assert!(s.diff.contains(".select(\"a\")"));
    }

    fn final_fix(source: &str, class: &str, attr: &str) -> Result<(Suggestion, String), String> {
        let module = parse_module(source).unwrap().into_syntax();
        let suggestion = suggest_final_constant(&module, source, "f.py", class, attr)?;
        let edited = crate::apply::apply_edits(source, &suggestion.edits).unwrap();
        Ok((suggestion, edited))
    }

    #[test]
    fn test_should_add_final_to_an_existing_typing_import_in_sorted_position() {
        // arrange
        let source = "from typing import Any, Optional\n\nimport pandas as pd\n\n\nclass Repo:\n    A = \"a\"\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert: one typing import, with `Final` where isort would put it
        assert!(
            edited.starts_with("from typing import Any, Final, Optional\n"),
            "{edited}"
        );
        assert_eq!(edited.matches("import Final").count(), 0, "{edited}");
    }

    #[test]
    fn test_should_keep_a_lazy_typing_import_lazy_when_adding_final() {
        // arrange
        let source = "lazy from typing import Any\n\nlazy import pandas as pd\n\n\nclass Repo:\n    A = \"a\"\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert
        assert!(
            edited.starts_with("lazy from typing import Any, Final\n"),
            "{edited}"
        );
        assert!(edited.contains("\nlazy import pandas as pd\n"), "{edited}");
        assert_eq!(edited.matches("import Final").count(), 0, "{edited}");
    }

    #[test]
    fn test_should_put_final_after_constants_and_before_functions_like_ruff_isort() {
        for (names, expected) in [
            ("TYPE_CHECKING, Any", "TYPE_CHECKING, Any, Final"),
            ("TYPE_CHECKING", "TYPE_CHECKING, Final"),
            ("Any, cast", "Any, Final, cast"),
            ("Generic, TypeVar", "Final, Generic, TypeVar"),
            ("cast", "Final, cast"),
        ] {
            // arrange
            let source = format!("from typing import {names}\n\n\nclass Repo:\n    A = \"a\"\n");

            // act
            let (_, edited) = final_fix(&source, "Repo", "A").unwrap();

            // assert
            assert!(
                edited.starts_with(&format!("from typing import {expected}\n")),
                "{edited}"
            );
        }
    }

    #[test]
    fn test_should_declare_a_plain_class_constant_final_and_import_final() {
        // arrange
        let source = "import pandas as pd\n\n\nclass Repo:\n    A = \"a\"\n";

        // act
        let (suggestion, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert
        assert_eq!(suggestion.kind, "final");
        assert_eq!(suggestion.tier, "safe");
        assert_eq!(
            edited,
            "from typing import Final\n\nimport pandas as pd\n\n\nclass Repo:\n    A: Final = \"a\"\n"
        );
    }

    #[test]
    fn test_should_wrap_an_existing_annotation_in_final() {
        // arrange
        let source = "from typing import Final\n\n\nclass Repo:\n    A: str = \"a\"\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert: `Final` was already imported, so no second import.
        assert_eq!(
            edited,
            "from typing import Final\n\n\nclass Repo:\n    A: Final[str] = \"a\"\n"
        );
    }

    #[test]
    fn test_should_reuse_the_typing_module_or_an_alias_instead_of_importing() {
        // arrange
        let module_import = "import typing as t\n\n\nclass Repo:\n    A = 1\n";
        let alias = "from typing import Final as F\n\n\nclass Repo:\n    A = 1\n";

        // act
        let (_, via_module) = final_fix(module_import, "Repo", "A").unwrap();
        let (_, via_alias) = final_fix(alias, "Repo", "A").unwrap();

        // assert
        assert!(via_module.contains("A: t.Final = 1"), "{via_module}");
        assert!(via_alias.contains("A: F = 1"), "{via_alias}");
    }

    #[test]
    fn test_should_import_final_after_a_docstring_and_future_import() {
        // arrange
        let source =
            "\"\"\"Doc.\"\"\"\nfrom __future__ import annotations\n\n\nclass Repo:\n    A = 1\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert
        assert!(
            edited.starts_with(
                "\"\"\"Doc.\"\"\"\nfrom __future__ import annotations\n\nfrom typing import Final\n"
            ),
            "{edited}"
        );
    }

    #[test]
    fn test_should_import_final_before_the_first_statement_when_there_are_no_imports() {
        // arrange
        let source = "\"\"\"Doc.\"\"\"\n\n\nclass Repo:\n    A = 1\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert
        assert!(
            edited.contains("\"\"\"Doc.\"\"\"\n\n\nfrom typing import Final\n\nclass Repo:"),
            "{edited}"
        );
    }

    #[test]
    fn test_should_withhold_a_constant_not_assigned_once_in_the_class_body() {
        // arrange
        let inherited = "from lib import Base\n\n\nclass Repo(Base):\n    pass\n";
        let twice = "class Repo:\n    A = 1\n    A = 2\n";
        let chained = "class Repo:\n    A = B = 1\n";
        let unassigned = "class Repo:\n    A: int\n";

        // act / assert
        for source in [inherited, twice, chained, unassigned] {
            assert!(final_fix(source, "Repo", "A").is_err(), "{source}");
        }
    }

    #[test]
    fn test_should_withhold_a_class_not_defined_once_at_the_top_level() {
        // arrange
        let source = "def make():\n    class Repo:\n        A = 1\n";

        // act
        let result = final_fix(source, "Repo", "A");

        // assert
        assert!(result.is_err());
    }

    #[test]
    fn test_should_show_code_between_two_edits_as_context_in_the_diff() {
        // arrange
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nX\nb\nc\nd\ne\nf\ng\nh\nY\n";

        // act
        let diff = render_unified_diff("f.py", old, new);

        // assert
        assert!(diff.contains("+X\n b\n c\n"), "{diff}");
        assert!(!diff.contains("-b"), "{diff}");
    }

    #[test]
    fn test_should_declare_final_on_the_base_class_that_defines_the_constant() {
        // arrange
        let source = "class Base:\n    TARGET = \"y\"\n\n\nclass Mid(Base):\n    pass\n\n\nclass Repo(Mid):\n    pass\n";

        // act
        let (suggestion, edited) = final_fix(source, "Repo", "TARGET").unwrap();

        // assert
        assert_eq!(suggestion.line, 2);
        assert!(edited.contains("    TARGET: Final = \"y\""), "{edited}");
        assert!(
            suggestion.reason.contains("`Base`"),
            "{}",
            suggestion.reason
        );
    }

    #[test]
    fn test_should_prefer_the_nearest_base_that_assigns_the_constant() {
        // arrange
        let source = "class Base:\n    T = 1\n\n\nclass Mid(Base):\n    T = 2\n\n\nclass Repo(Mid):\n    pass\n";

        // act
        let result = final_fix(source, "Repo", "T");

        // assert: Mid defines it, but it rebinds Base's, which Final on Mid does not touch.
        let (suggestion, _) = result.unwrap();
        assert_eq!(suggestion.line, 6);
    }

    #[test]
    fn test_should_withhold_when_a_subclass_in_the_file_rebinds_the_constant() {
        // arrange
        let source = "class Base:\n    T = 1\n\n\nclass Child(Base):\n    T = 2\n";

        // act
        let result = final_fix(source, "Base", "T");

        // assert
        let reason = result.map(|_| ()).unwrap_err();
        assert!(reason.contains("`Child` rebinds `T`"), "{reason}");
    }

    #[test]
    fn test_should_withhold_when_an_earlier_base_is_defined_in_another_file() {
        // arrange
        let source = "from lib import Mixin\n\n\nclass Base:\n    T = 1\n\n\nclass Repo(Mixin, Base):\n    pass\n";

        // act
        let result = final_fix(source, "Repo", "T");

        // assert
        assert!(result.is_err());
    }

    #[test]
    fn test_should_insert_the_import_on_its_own_line_after_a_trailing_comment() {
        // arrange
        let source = "import pandas as pd  # noqa: F401\n\n\nclass Repo:\n    A = 1\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert: the pragma stays on the import it belongs to.
        assert!(
            edited.starts_with("from typing import Final\n\nimport pandas as pd  # noqa: F401\n"),
            "{edited}"
        );
    }

    #[test]
    fn test_should_insert_the_import_after_everything_sharing_the_last_import_line() {
        // arrange
        let source = "import os; x = 1\n\n\nclass Repo:\n    A = 1\n";
        let continued = "import os; x = (\n    1\n)\n\n\nclass Repo:\n    A = 1\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();
        let (_, edited_continued) = final_fix(continued, "Repo", "A").unwrap();

        // assert
        assert!(
            edited.starts_with("import os; x = 1\nfrom typing import Final\n"),
            "{edited}"
        );
        assert!(
            edited_continued.starts_with("import os; x = (\n    1\n)\nfrom typing import Final\n"),
            "{edited_continued}"
        );
    }

    #[test]
    fn test_should_use_the_files_line_ending_for_the_import() {
        // arrange
        let source = "import os\r\n\r\n\r\nclass Repo:\r\n    A = 1\r\n";
        let bare = "\"\"\"Doc.\"\"\"\r\n\r\n\r\nclass Repo:\r\n    A = 1\r\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();
        let (_, edited_bare) = final_fix(bare, "Repo", "A").unwrap();

        // assert: no bare `\n` among the CRLFs.
        for text in [edited, edited_bare] {
            assert!(text.contains("from typing import Final\r\n"), "{text:?}");
            assert_eq!(
                text.matches('\n').count(),
                text.matches("\r\n").count(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn test_should_turn_a_class_var_annotation_into_final() {
        // arrange
        let source = "from typing import ClassVar, Final\n\n\nclass Repo:\n    A: ClassVar[str] = \"a\"\n    B: ClassVar = \"b\"\n";

        // act
        let (_, edited_a) = final_fix(source, "Repo", "A").unwrap();
        let (_, edited_b) = final_fix(source, "Repo", "B").unwrap();

        // assert
        assert!(edited_a.contains("A: Final[str] = \"a\""), "{edited_a}");
        assert!(edited_b.contains("B: Final = \"b\""), "{edited_b}");
    }

    #[test]
    fn test_should_drop_the_class_var_import_once_nothing_uses_it() {
        // arrange
        let one = "from typing import ClassVar\n\nimport pandas as pd\n\n\nclass T:\n    A: ClassVar[list[str]] = [\"a\"]\n";
        let kept = "from typing import Any, ClassVar\n\n\nclass T:\n    A: ClassVar[list[str]] = [\"a\"]\n    B: ClassVar[list[str]] = [\"b\"]\n";
        let several = "from typing import ClassVar, Final\n\n\nclass T:\n    A: ClassVar[list[str]] = [\"a\"]\n    B: ClassVar[list[str]] = [\"b\"]\n";

        // act
        let (_, only) = final_fix(one, "T", "A").unwrap();
        let (_, shared) = final_fix(kept, "T", "A").unwrap();
        let both = without_orphaned_class_var(several, &several.replace("ClassVar[", "Final["));

        // assert
        assert!(only.starts_with("from typing import Final\n"), "{only}");
        assert!(shared.contains("import Any, ClassVar, Final"), "{shared}");
        assert!(both.starts_with("from typing import Final\n"), "{both}");
    }

    #[test]
    fn test_should_leave_an_already_unused_class_var_import_alone() {
        // arrange
        let source = "from typing import ClassVar, Final\n\n\nclass T:\n    A: Final = 1\n";

        // act / assert
        assert_eq!(source, without_orphaned_class_var(source, source));
    }

    #[test]
    fn test_should_render_two_distant_edits_as_two_hunks() {
        // arrange
        let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let new = old.replacen("line 1\n", "line one\n", 1).replacen(
            "line 35\n",
            "line thirty-five\n",
            1,
        );

        // act
        let diff = render_unified_diff("f.py", &old, &new);

        // assert
        assert_eq!(2, diff.matches("@@ -").count(), "{diff}");
        assert!(diff.contains("-line 35\n+line thirty-five\n"), "{diff}");
        assert!(!diff.contains("line 15"), "{diff}");
    }

    #[test]
    fn test_should_join_hunks_whose_context_meets() {
        // arrange
        let old: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replacen("line 5\n", "five\n", 1)
            .replacen("line 9\n", "nine\n", 1);

        // act
        let diff = render_unified_diff("f.py", &old, &new);

        // assert
        assert_eq!(1, diff.matches("@@ -").count(), "{diff}");
    }

    #[test]
    fn test_should_withhold_when_final_is_already_something_else() {
        // arrange
        let assigned = "import os\n\nFinal = 3\n\n\nclass Repo:\n    A = 1\n";
        let imported = "from other import Final\n\n\nclass Repo:\n    A = 1\n";

        // act / assert
        for source in [assigned, imported] {
            let reason = final_fix(source, "Repo", "A").map(|_| ()).unwrap_err();
            assert!(reason.contains("`Final`"), "{reason}");
        }
    }

    #[test]
    fn test_should_put_the_import_in_the_standard_library_group_in_sorted_position() {
        // arrange
        let cls = "\n\nclass Repo:\n    A = 1\n";
        let before_third_party =
            format!("import os\nfrom pathlib import Path\n\nimport pandas as pd\n{cls}");
        let between = format!("import os\nfrom collections import Counter\nfrom zlib import crc32\n\nimport pandas as pd\n{cls}");
        let first_from = format!(
            "from pathlib import Path\nfrom zipfile import ZipFile\n\nimport pandas as pd\n{cls}"
        );

        // act
        let (_, a) = final_fix(&before_third_party, "Repo", "A").unwrap();
        let (_, b) = final_fix(&between, "Repo", "A").unwrap();
        let (_, c) = final_fix(&first_from, "Repo", "A").unwrap();

        // assert: after the last smaller module, before the first larger one, no stray blank lines
        assert!(
            a.starts_with(
                "import os\nfrom pathlib import Path\nfrom typing import Final\n\nimport pandas"
            ),
            "{a}"
        );
        assert!(b.starts_with("import os\nfrom collections import Counter\nfrom typing import Final\nfrom zlib import crc32\n\nimport pandas"), "{b}");
        assert!(c.starts_with("from pathlib import Path\nfrom typing import Final\nfrom zipfile import ZipFile\n\nimport pandas"), "{c}");
        for edited in [a, b, c] {
            assert!(imports_in_order(&edited), "{edited}");
        }
    }

    #[test]
    fn test_should_start_a_group_of_its_own_before_third_party_imports() {
        // arrange
        let source = "import numpy as np\nimport pandas as pd\n\nfrom myapp import util\n\n\nclass Repo:\n    A = 1\n";

        // act
        let (_, edited) = final_fix(source, "Repo", "A").unwrap();

        // assert
        assert!(
            edited.starts_with("from typing import Final\n\nimport numpy as np\nimport pandas as pd\n\nfrom myapp import util\n"),
            "{edited}"
        );
        assert!(imports_in_order(&edited));
    }

    #[test]
    fn test_should_tell_ordered_import_blocks_from_unordered_ones() {
        // arrange / act / assert
        assert!(imports_in_order(
            "import os\nimport sys\n\nimport pandas as pd\n"
        ));
        assert!(!imports_in_order("import pandas as pd\nimport os\n"));
        assert!(!imports_in_order(
            "import pandas as pd\nfrom typing import Final\n"
        ));
        assert!(!imports_in_order(
            "import os\nfrom typing import Final\nfrom pathlib import Path\n"
        ));
        assert!(!imports_in_order("import os\n\nimport sys\n"));
    }

    #[test]
    fn test_should_read_the_columns_of_a_selected_groupby_and_a_literal_rename() {
        // arrange
        let source = "items = pd.read_parquet(\"i.parquet\")\nitems[\"price\"] = items[\"price\"].fillna(items.groupby(\"category\")[\"price\"].transform(\"median\"))\nitems = items.rename(columns={\"quality_score\": \"qscore\"})\nprint(items[\"qscore\"].mean())\n";

        // act
        let suggestion = run(source, false).unwrap();

        // assert: the group key, the selected column and the renamed source are read; the new
        // name is not a column of the file
        assert_eq!(suggestion.columns, ["price", "category", "quality_score"]);
    }

    #[test]
    fn test_should_read_the_sources_of_a_named_aggregation() {
        // arrange
        let source = "items = pd.read_parquet(\"i.parquet\")\nseg = items.groupby(\"category\").agg(avg=(\"price\", \"mean\"))\nprint(seg)\n";

        // act
        let suggestion = run(source, false).unwrap();

        // assert
        assert_eq!(suggestion.columns, ["category", "price"]);
    }

    #[test]
    fn test_should_still_block_a_groupby_whose_result_reads_every_column() {
        // arrange
        let source = "items = pd.read_parquet(\"i.parquet\")\ntotals = items.groupby(\"category\").sum()\nprint(items[\"price\"])\n";

        // act
        let reason = run(source, false).unwrap_err();

        // assert
        assert!(reason.contains(".groupby()"), "{reason}");
    }
}
