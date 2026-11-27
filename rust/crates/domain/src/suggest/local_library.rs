use super::*;

// Pandas or polars. In order of how direct the evidence is: the variable's own annotation;
// the method that produced the frame (`.to_df()`, `.fetch_pandas_all()`, `.toPandas()` are
// pandas, `.pl()` polars); the module or imported name the expression starts from
// (`pd.read_parquet(...)`, `read_csv(...)` imported from pandas, connectorx's
// `return_type=`); the return annotation of a function it calls.
pub(super) fn library_of(origin: &Origin) -> Result<Library, String> {
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
    let mut finder = Finder {
        name,
        before,
        found: Vec::new(),
    };
    ast::visitor::source_order::SourceOrderVisitor::visit_body(&mut finder, body);
    finder.found
}

struct Finder<'a, 'n> {
    pub(super) name: &'n str,
    pub(super) before: usize,
    pub(super) found: Vec<(Option<&'a Expr>, Option<&'a Expr>)>,
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

// The library of `id`, a local of the origin's scope bound before `before`: the one every
// earlier assignment to it agrees on.
pub(super) fn local_library(
    origin: &Origin,
    id: &str,
    before: usize,
    depth: usize,
) -> Option<Library> {
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
                let evidence = match library_of_instance(
                    origin,
                    value,
                    depth,
                    innermost_call,
                    root_attribute,
                    attribute_before_root,
                    id,
                ) {
                    Ok(value) => value,
                    Err(value) => return value,
                };
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

fn library_of_instance(
    origin: &Origin<'_>,
    value: &Expr,
    depth: usize,
    innermost_call: Option<&ruff_python_ast::ExprCall>,
    root_attribute: Option<&str>,
    attribute_before_root: Option<&str>,
    id: &str,
) -> Result<String, Result<Library, String>> {
    let context = origin.libraries;
    if let (true, Some(method), Some(attr), true) = (
        id == "self",
        attribute_before_root,
        root_attribute,
        innermost_call.is_some(),
    ) {
        if let Some(library) = origin
            .receivers
            .get(&format!("self.{attr}"))
            .and_then(|class| {
                context
                    .class_methods
                    .get(&(class.clone(), method.to_string()))
            })
        {
            return Err(Ok(*library));
        }
    }
    if matches!(id, "self" | "cls") && innermost_call.is_some() {
        if let Some(library) = root_attribute.and_then(|m| context.functions.get(m)) {
            return Err(Ok(*library));
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
            return Err(Ok(*library));
        }
    }
    match context.modules.get(id) {
        Some(ModuleKind::Library(library)) => return Err(Ok(*library)),
        Some(ModuleKind::Connectorx) => return Err(Ok(connectorx_library(innermost_call))),
        None => {}
    }
    if let Some(library) = context.names.get(id).or_else(|| context.functions.get(id)) {
        return Err(Ok(*library));
    }
    if let Some(library) = local_library(origin, id, value.range().start().to_usize(), depth) {
        return Err(Ok(library));
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
    Ok(evidence)
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
