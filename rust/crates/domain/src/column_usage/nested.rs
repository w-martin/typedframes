use super::*;

/// The offset of the first mention of any of `names` inside a nested function, class,
/// lambda or generator expression anywhere in `body` -- the scan above deliberately does not
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
    pub(super) names: &'n HashSet<String>,
    pub(super) depth: u32,
    pub(super) first: Option<TextSize>,
}

impl NestedMentionFinder<'_> {
    pub(super) fn visit_parameter_defaults(&mut self, parameters: &ast::Parameters) {
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
            // A generator runs later, perhaps once the name holds another frame; an eager
            // comprehension runs here and `UsageScan` walks it.
            Expr::Generator(c) => self.visit_comprehension(&c.generators, &[&c.elt]),
            _ => ast_visitor::walk_expr(self, expr),
        }
    }
}

struct ScopeDeclFinder<'n> {
    pub(super) name: &'n str,
    pub(super) found: bool,
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

// Whether assigning to `target` rebinds `name` (not `name["col"] = ..` or `name.attr = ..`).
pub(super) fn binds(target: &Expr, name: &str) -> bool {
    match target {
        Expr::Name(n) => n.id.as_str() == name,
        Expr::Tuple(t) => t.elts.iter().any(|e| binds(e, name)),
        Expr::List(l) => l.elts.iter().any(|e| binds(e, name)),
        Expr::Starred(s) => binds(&s.value, name),
        _ => false,
    }
}

// "passed to `clean()`" for `clean(df)`, "passed to `np.log()`" for `np.log(df)`, and the
// generic "passed to a call" when the callee isn't a plain name or `module.name`.
pub(super) fn passed_to(func: &Expr) -> String {
    match func {
        Expr::Name(name) => format!("passed to `{}()`", name.id),
        Expr::Attribute(attr) => match &*attr.value {
            Expr::Name(base) => format!("passed to `{}.{}()`", base.id, attr.attr),
            _ => "passed to a call".to_string(),
        },
        _ => "passed to a call".to_string(),
    }
}
