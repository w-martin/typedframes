use super::*;

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
    pub(super) fn extend(&mut self, body: &[Stmt]) {
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
            self.collect_statement_imports(stmt);
        }
    }

    fn collect_statement_imports(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Import(import) => {
                self.collect_import(import);
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

    fn collect_import(&mut self, import: &ruff_python_ast::StmtImport) {
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

    /// The library an annotation names (`pd.DataFrame`, `pl.LazyFrame`, an imported
    /// `DataFrame`), unwrapping `Optional`/`Annotated`/`X | None`.
    pub(super) fn annotation_library(&self, annotation: &Expr) -> Option<Library> {
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
                    collect_method_return(context, out, class, method, plain);
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

fn collect_method_return(
    context: &LibraryContext,
    out: &mut HashMap<(String, String), Option<Library>>,
    class: &ruff_python_ast::StmtClassDef,
    method: &ruff_python_ast::StmtFunctionDef,
    plain: bool,
) {
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
