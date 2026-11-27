use super::*;

impl<'r> Evaluator<'r> {
    pub(super) fn binop(&self, left: &Expr, op: Operator, right: &Expr) -> Option<Value> {
        let l = self.ev(left)?;
        let r = self.ev(right)?;
        let result = match (op, &l, &r) {
            (Operator::Add, Value::Str(a), Value::Str(b)) => Value::Str(format!("{a}{b}")),
            (Operator::Add, Value::List(a), Value::List(b)) => {
                Value::List(a.iter().chain(b).cloned().collect())
            }
            (Operator::Add, Value::Tuple(a), Value::Tuple(b)) => {
                Value::Tuple(a.iter().chain(b).cloned().collect())
            }
            (Operator::Add, a, b) if a.number().is_some() && b.number().is_some() => {
                Value::Int(a.number()?.checked_add(b.number()?)?)
            }
            (Operator::Sub, a, b) if a.number().is_some() && b.number().is_some() => {
                Value::Int(a.number()?.checked_sub(b.number()?)?)
            }
            (Operator::Mult, a, b) if a.number().is_some() && b.number().is_some() => {
                Value::Int(a.number()?.checked_mul(b.number()?)?)
            }
            (Operator::Mult, seq, n) | (Operator::Mult, n, seq)
                if matches!(seq, Value::Str(_) | Value::List(_) | Value::Tuple(_))
                    && n.number().is_some() =>
            {
                let times = usize::try_from(n.number()?.max(0)).ok()?;
                match seq {
                    Value::Str(s) => {
                        (s.len().checked_mul(times)? <= MAX_STRING).then_some(())?;
                        Value::Str(s.repeat(times))
                    }
                    Value::List(v) | Value::Tuple(v) => {
                        (v.len().checked_mul(times)? <= MAX_ELEMENTS).then_some(())?;
                        let repeated: Vec<Value> =
                            std::iter::repeat_n(v.clone(), times).flatten().collect();
                        if matches!(seq, Value::List(_)) {
                            Value::List(repeated)
                        } else {
                            Value::Tuple(repeated)
                        }
                    }
                    _ => return None,
                }
            }
            (Operator::BitOr, Value::Set(a), Value::Set(b)) => {
                let mut out = a.clone();
                for v in b {
                    set_insert(&mut out, v.clone());
                }
                Value::Set(out)
            }
            (Operator::BitAnd, Value::Set(a), Value::Set(b)) => Value::Set(
                a.iter()
                    .filter(|x| b.iter().any(|y| x.py_eq(y)))
                    .cloned()
                    .collect(),
            ),
            (Operator::Sub, Value::Set(a), Value::Set(b)) => Value::Set(
                a.iter()
                    .filter(|x| !b.iter().any(|y| x.py_eq(y)))
                    .cloned()
                    .collect(),
            ),
            (Operator::BitOr, Value::Dict(a), Value::Dict(b)) => {
                let mut out = a.clone();
                for (k, v) in b {
                    dict_insert(&mut out, k.clone(), v.clone());
                }
                Value::Dict(out)
            }
            _ => return None,
        };
        Self::capped(result)
    }

    pub(super) fn compare(&self, cmp: &ast::ExprCompare) -> Option<Value> {
        let mut left = self.ev(&cmp.left)?;
        for (op, comparator) in cmp.ops.iter().zip(cmp.comparators.iter()) {
            let right = self.ev(comparator)?;
            let holds = match op {
                CmpOp::Eq => left.py_eq(&right),
                CmpOp::NotEq => !left.py_eq(&right),
                CmpOp::Lt => left.compare(&right)? == Ordering::Less,
                CmpOp::LtE => left.compare(&right)? != Ordering::Greater,
                CmpOp::Gt => left.compare(&right)? == Ordering::Greater,
                CmpOp::GtE => left.compare(&right)? != Ordering::Less,
                CmpOp::In => contains(&right, &left)?,
                CmpOp::NotIn => !contains(&right, &left)?,
                // Identity is only decidable for `None`.
                CmpOp::Is => none_identity(&left, &right)?,
                CmpOp::IsNot => !none_identity(&left, &right)?,
            };
            if !holds {
                return Some(Value::Bool(false));
            }
            left = right;
        }
        Some(Value::Bool(true))
    }

    pub(super) fn subscript(&self, sub: &ast::ExprSubscript) -> Option<Value> {
        if sub.ctx != ExprContext::Load {
            return None;
        }
        let base = self.ev(&sub.value)?;
        if let Expr::Slice(slice) = &*sub.slice {
            let bound = |e: &Option<Box<Expr>>| -> Option<Option<i64>> {
                match e {
                    None => Some(None),
                    Some(e) => match self.ev(e)? {
                        Value::None => Some(None),
                        v => Some(Some(v.number()?)),
                    },
                }
            };
            let (lower, upper, step) = (
                bound(&slice.lower)?,
                bound(&slice.upper)?,
                bound(&slice.step)?,
            );
            return slice_value(&base, lower, upper, step);
        }
        let index = self.ev(&sub.slice)?;
        match (&base, &index) {
            (Value::List(v) | Value::Tuple(v), i) => {
                let i = normalize_index(i.number()?, v.len())?;
                v.get(i).cloned()
            }
            (Value::Str(s), i) => {
                let chars: Vec<char> = s.chars().collect();
                let i = normalize_index(i.number()?, chars.len())?;
                chars.get(i).map(|c| Value::Str(c.to_string()))
            }
            (Value::Dict(d), key) => d.iter().find(|(k, _)| k.py_eq(key)).map(|(_, v)| v.clone()),
            _ => None,
        }
    }

    pub(super) fn comprehension(
        &self,
        generators: &[ast::Comprehension],
        ordered: bool,
        emit: &mut dyn FnMut() -> Option<()>,
    ) -> Option<()> {
        self.locals.borrow_mut().push(HashMap::new());
        let result = self.comprehension_level(generators, 0, ordered, emit);
        self.locals.borrow_mut().pop();
        result
    }

    fn comprehension_level(
        &self,
        generators: &[ast::Comprehension],
        index: usize,
        ordered: bool,
        emit: &mut dyn FnMut() -> Option<()>,
    ) -> Option<()> {
        let Some(generator) = generators.get(index) else {
            return emit();
        };
        if generator.is_async {
            return None;
        }
        let source = self.arg(&generator.iter, ordered)?;
        let items = if ordered {
            Self::iterate(&source)?
        } else {
            Self::iterate_unordered(&source)?
        };
        for item in items {
            self.tick()?;
            self.bind(&generator.target, item)?;
            let mut keep = true;
            for condition in &generator.ifs {
                if !self.ev(condition)?.truthy() {
                    keep = false;
                    break;
                }
            }
            if keep {
                self.comprehension_level(generators, index + 1, ordered, emit)?;
            }
        }
        Some(())
    }

    pub(super) fn bind(&self, target: &Expr, value: Value) -> Option<()> {
        match target {
            Expr::Name(n) => {
                self.locals
                    .borrow_mut()
                    .last_mut()?
                    .insert(n.id.to_string(), value);
                Some(())
            }
            Expr::Tuple(t) => self.bind_all(&t.elts, value),
            Expr::List(l) => self.bind_all(&l.elts, value),
            _ => None,
        }
    }

    fn bind_all(&self, targets: &[Expr], value: Value) -> Option<()> {
        let items = match value {
            Value::List(v) | Value::Tuple(v) => v,
            _ => return None,
        };
        if items.len() != targets.len() || targets.iter().any(|t| matches!(t, Expr::Starred(_))) {
            return None;
        }
        for (target, item) in targets.iter().zip(items) {
            self.bind(target, item)?;
        }
        Some(())
    }

    pub(super) fn call(&self, call: &ast::ExprCall) -> Option<Value> {
        match &*call.func {
            Expr::Name(f) => match self.resolver.resolve(f) {
                // Only a name nothing in the file binds is the builtin.
                Resolved::Builtin => self.builtin(f.id.as_str(), call),
                _ => self.user_call(f, call),
            },
            Expr::Attribute(a) => {
                if self.resolver.is_module_member(a) {
                    let args = self.positional_exact(call)?;
                    let mut kwargs = Vec::new();
                    for keyword in call.arguments.keywords.iter() {
                        kwargs.push((keyword.arg.as_ref()?.to_string(), self.ev(&keyword.value)?));
                    }
                    return self.resolver.call_module_function(a, args, kwargs);
                }
                let receiver = self.ev(&a.value)?;
                self.method(receiver, a.attr.as_str(), call)
            }
            _ => None,
        }
    }

    fn user_call(&self, callee: &ast::ExprName, call: &ast::ExprCall) -> Option<Value> {
        let args = self.positional_exact(call)?;
        let mut kwargs = Vec::new();
        for keyword in call.arguments.keywords.iter() {
            kwargs.push((keyword.arg.as_ref()?.to_string(), self.ev(&keyword.value)?));
        }
        self.resolver.call_function(callee, args, kwargs)
    }

    /// Runs a straight-line function body -- `x = expr` statements, an optional
    /// docstring, and a final `return expr` -- with `params` bound, and returns what it
    /// returns. Anything else in the body (a loop, a branch, a bare call, a `global`)
    /// makes the function not evaluable.
    pub(crate) fn run_body(
        &self,
        params: HashMap<String, Value>,
        body: &[ast::Stmt],
    ) -> Option<Value> {
        use ast::Stmt;
        self.steps.set(0);
        self.depth.set(0);
        *self.locals.borrow_mut() = vec![params];
        let mut result = None;
        for stmt in body {
            if result.is_some() {
                return None;
            }
            match stmt {
                Stmt::Pass(_) => {}
                Stmt::Expr(e) if matches!(*e.value, Expr::StringLiteral(_)) => {}
                Stmt::Assign(a) => {
                    let [Expr::Name(target)] = a.targets.as_slice() else {
                        return None;
                    };
                    let value = self.ev(&a.value)?;
                    self.locals
                        .borrow_mut()
                        .last_mut()?
                        .insert(target.id.to_string(), value);
                }
                Stmt::AnnAssign(a) => {
                    let Expr::Name(target) = &*a.target else {
                        return None;
                    };
                    let value = self.ev(a.value.as_deref()?)?;
                    self.locals
                        .borrow_mut()
                        .last_mut()?
                        .insert(target.id.to_string(), value);
                }
                Stmt::Return(r) => {
                    result = Some(match &r.value {
                        Some(value) => self.ev(value)?,
                        None => Value::None,
                    });
                }
                _ => return None,
            }
        }
        result
    }

    // Positional arguments of a builtin that consumes iterables (so a generator or
    // `zip` is fine). A keyword or `*args` is handled by the few that accept one.
    pub(super) fn positional(&self, call: &ast::ExprCall, ordered: bool) -> Option<Vec<Value>> {
        call.arguments
            .args
            .iter()
            .map(|a| {
                if matches!(a, Expr::Starred(_)) {
                    None
                } else {
                    self.arg(a, ordered)
                }
            })
            .collect()
    }

    // Arguments that must already be values: a one-shot iterator is refused, since the
    // callee may use it as something else or more than once.
    pub(super) fn positional_exact(&self, call: &ast::ExprCall) -> Option<Vec<Value>> {
        call.arguments
            .args
            .iter()
            .map(|a| {
                if matches!(a, Expr::Starred(_)) {
                    None
                } else {
                    self.ev(a)
                }
            })
            .collect()
    }

    pub(super) fn keyword(&self, call: &ast::ExprCall, name: &str) -> Option<Option<Value>> {
        let mut found = None;
        for kw in call.arguments.keywords.iter() {
            if kw.arg.as_ref().map(|a| a.as_str()) == Some(name) {
                found = Some(self.ev(&kw.value)?);
            }
        }
        Some(found)
    }

    pub(super) fn only_keywords(call: &ast::ExprCall, allowed: &[&str]) -> bool {
        call.arguments.keywords.iter().all(|k| {
            k.arg
                .as_ref()
                .is_some_and(|a| allowed.contains(&a.as_str()))
        })
    }
}
