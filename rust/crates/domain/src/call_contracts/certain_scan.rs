use super::*;

pub(super) struct CertainScan {
    pub(super) aliases: HashMap<String, usize>,
    pub(super) done: Vec<bool>,
    pub(super) reads: Vec<Vec<String>>,
    pub(super) forwards: Vec<Forward>,
    // Per statement: names whose every occurrence was accounted for, and params to stop.
    pub(super) accounted: HashSet<u32>,
    pub(super) stop: Vec<usize>,
}

impl CertainScan {
    pub(super) fn new(names: &[String]) -> Self {
        CertainScan {
            aliases: names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), i))
                .collect(),
            done: vec![false; names.len()],
            reads: vec![Vec::new(); names.len()],
            forwards: Vec::new(),
            accounted: HashSet::new(),
            stop: Vec::new(),
        }
    }

    fn alias(&self, expr: &Expr) -> Option<usize> {
        let Expr::Name(name) = expr else {
            return None;
        };
        let param = *self.aliases.get(name.id.as_str())?;
        (!self.done[param]).then_some(param)
    }

    pub(super) fn body(&mut self, body: &[Stmt]) {
        for stmt in body {
            if self.done.iter().all(|d| *d) || !self.statement(stmt) {
                return;
            }
        }
    }

    // Scans one top-level statement; `false` once later statements may not run.
    pub(super) fn statement(&mut self, stmt: &Stmt) -> bool {
        self.accounted.clear();
        self.stop.clear();
        let mut mentions = Mentions {
            aliases: &self.aliases,
            found: Vec::new(),
        };
        mentions.visit_stmt(stmt);
        let found = mentions.found;
        let go_on = match stmt {
            Stmt::Expr(e) => {
                self.expr(&e.value);
                true
            }
            Stmt::Return(r) => {
                if let Some(value) = &r.value {
                    self.expr(value);
                }
                false
            }
            Stmt::Assign(assign) => self.statement_assign(assign),
            Stmt::AnnAssign(assign) => {
                let forwarded = assign.value.as_deref().and_then(|v| self.forwarded(v));
                if let Some(value) = &assign.value {
                    self.expr(value);
                }
                self.target(&assign.target);
                if let (Expr::Name(t), Some(param)) = (&*assign.target, forwarded) {
                    self.aliases.insert(t.id.to_string(), param);
                    self.done[param] = false;
                }
                true
            }
            Stmt::AugAssign(assign) => {
                self.expr(&assign.value);
                if let Expr::Subscript(sub) = &*assign.target {
                    self.read(sub);
                }
                self.target(&assign.target);
                true
            }
            Stmt::If(if_stmt) => {
                self.expr(&if_stmt.test);
                false
            }
            Stmt::Assert(assert) => {
                self.expr(&assert.test);
                true
            }
            Stmt::Pass(_) => true,
            _ => false,
        };
        // Any other mention (a lambda, a container, a branch of a conditional) may let the
        // frame change in a way this scan doesn't follow.
        for (at, param) in found {
            if !self.accounted.contains(&at) {
                self.stop.push(param);
            }
        }
        for param in std::mem::take(&mut self.stop) {
            self.done[param] = true;
        }
        go_on
    }

    fn statement_assign(&mut self, assign: &ruff_python_ast::StmtAssign) -> bool {
        let forwarded = self.forwarded(&assign.value);
        self.expr(&assign.value);
        for target in &assign.targets {
            self.target(target);
        }
        if let Some(param) = forwarded {
            for target in &assign.targets {
                if let Expr::Name(t) = target {
                    self.aliases.insert(t.id.to_string(), param);
                    self.done[param] = false;
                }
            }
        }
        true
    }

    // `x = param` or `x = param.copy()`: `x` is the same frame (or a copy of it).
    pub(super) fn forwarded(&mut self, value: &Expr) -> Option<usize> {
        let param = match value {
            Expr::Name(_) => self.alias(value),
            Expr::Call(call)
                if call.arguments.is_empty()
                    && matches!(&*call.func, Expr::Attribute(a) if a.attr.as_str() == "copy") =>
            {
                let Expr::Attribute(attr) = &*call.func else {
                    return None;
                };
                self.alias(&attr.value)
            }
            _ => None,
        };
        if param.is_some() {
            let at = match value {
                Expr::Call(call) => call.func.range().start(),
                _ => value.range().start(),
            };
            self.accounted.insert(at.to_u32());
        }
        param
    }

    // A store: rebinding a name ends its aliasing, writing through one ends its parameter.
    pub(super) fn target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => {
                if let Some(param) = self.aliases.remove(name.id.as_str()) {
                    if self.aliases.values().all(|p| *p != param) {
                        self.done[param] = true;
                    }
                }
            }
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.target(e)),
            Expr::List(l) => l.elts.iter().for_each(|e| self.target(e)),
            Expr::Starred(s) => self.target(&s.value),
            _ => {
                if let Some(param) = root_name(target).and_then(|n| self.aliases.get(n).copied()) {
                    self.done[param] = true;
                }
            }
        }
    }

    pub(super) fn read(&mut self, sub: &ast::ExprSubscript) {
        let Some(param) = self.alias(&sub.value) else {
            return;
        };
        self.accounted.insert(sub.value.range().start().to_u32());
        if let Some(columns) = crate::ast_extract::extract_string_list_or_single(&sub.slice) {
            for column in columns {
                if !self.reads[param].contains(&column) {
                    self.reads[param].push(column);
                }
            }
        }
    }

    // Walks the parts of `expr` that are evaluated whenever it is.
    pub(super) fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Subscript(sub) => {
                if sub.ctx == ast::ExprContext::Load {
                    self.read(sub);
                }
                self.expr(&sub.value);
                self.expr(&sub.slice);
            }
            Expr::Attribute(attr) => {
                if let Some(_param) = self.alias(&attr.value) {
                    self.accounted.insert(attr.value.range().start().to_u32());
                }
                self.expr(&attr.value);
            }
            Expr::Call(call) => self.call(call),
            Expr::BinOp(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expr::UnaryOp(u) => self.expr(&u.operand),
            Expr::BoolOp(b) => {
                if let Some(first) = b.values.first() {
                    self.expr(first);
                }
            }
            Expr::Compare(c) => {
                self.expr(&c.left);
                for comparator in c.comparators.iter() {
                    self.expr(comparator);
                }
            }
            Expr::If(e) => self.expr(&e.test),
            Expr::List(l) => l.elts.iter().for_each(|e| self.expr(e)),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.expr(e)),
            Expr::Name(_) => {
                // A bare alias here is one of the "other mentions" handled after the statement.
            }
            _ => {}
        }
    }

    fn polars_expression_receiver(&self, func: &Expr) -> Option<usize> {
        let Expr::Attribute(attr) = func else {
            return None;
        };
        match attr.attr.as_str() {
            "with_columns" | "select" | "filter" | "sort" | "group_by" => self.alias(&attr.value),
            "agg" => match &*attr.value {
                Expr::Call(grouped) => match &*grouped.func {
                    Expr::Attribute(g) if g.attr.as_str() == "group_by" => self.alias(&g.value),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn call(&mut self, call: &ast::ExprCall) {
        if let Expr::Attribute(attr) = &*call.func {
            if let Some(param) = self.alias(&attr.value) {
                let in_place = IN_PLACE_METHODS.contains(&attr.attr.as_str())
                    || call.arguments.keywords.iter().any(|k| {
                        k.arg
                            .as_ref()
                            .is_some_and(|a| matches!(a.as_str(), "inplace" | "in_place"))
                    });
                if in_place {
                    self.stop.push(param);
                }
            }
        }
        // `param.select(pl.col("a"))`, `param.group_by(..).agg(pl.col("a"))`: polars resolves
        // every column an expression names against `param`.
        if let Some(param) = self.polars_expression_receiver(&call.func) {
            let arguments = call
                .arguments
                .args
                .iter()
                .chain(call.arguments.keywords.iter().map(|k| &k.value));
            for column in arguments.flat_map(crate::ast_extract::collect_pl_col_names) {
                if !column.starts_with('^') && column != "*" && !self.reads[param].contains(&column)
                {
                    self.reads[param].push(column);
                }
            }
        }
        self.expr(&call.func);
        let harmless =
            matches!(&*call.func, Expr::Name(n) if HARMLESS_CALLS.contains(&n.id.as_str()));
        let shape = CallShape::of(call);
        let target = match &*call.func {
            Expr::Name(_) => Some(ForwardTarget::Call(call.func.range().start().to_u32())),
            Expr::Attribute(attr) => match &*attr.value {
                Expr::Name(base) if base.id.as_str() == "self" => {
                    Some(ForwardTarget::SelfMethod(attr.attr.to_string()))
                }
                Expr::Name(base) if !self.aliases.contains_key(base.id.as_str()) => {
                    Some(ForwardTarget::Call(call.func.range().start().to_u32()))
                }
                _ => None,
            },
            _ => None,
        };
        let arguments = call
            .arguments
            .args
            .iter()
            .chain(call.arguments.keywords.iter().map(|k| &k.value));
        for (index, argument) in arguments.enumerate() {
            if let Some(param) = self.alias(argument) {
                self.accounted.insert(argument.range().start().to_u32());
                if harmless {
                    continue;
                }
                // The callee may change the frame, so what follows is no longer certain.
                self.stop.push(param);
                if let (Some(target), Some(shape)) = (&target, &shape) {
                    self.forwards.push(Forward {
                        param,
                        target: target.clone(),
                        shape: shape.clone(),
                        argument: index,
                    });
                }
            } else {
                self.expr(argument);
            }
        }
    }
}

fn root_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => root_name(&a.value),
        Expr::Subscript(s) => root_name(&s.value),
        Expr::Call(c) => root_name(&c.func),
        _ => None,
    }
}

struct Mentions<'m> {
    pub(super) aliases: &'m HashMap<String, usize>,
    pub(super) found: Vec<(u32, usize)>,
}

impl<'a> Visitor<'a> for Mentions<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(name) = expr {
            if name.ctx == ast::ExprContext::Load {
                if let Some(param) = self.aliases.get(name.id.as_str()) {
                    self.found.push((name.range().start().to_u32(), *param));
                }
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

/// The key a method or function is stored under in the project index.
pub(crate) fn index_key(key: &FnKey) -> String {
    match &key.0 {
        Some(class) => format!("{class}.{}", key.1),
        None => key.1.clone(),
    }
}
