use super::*;

pub(crate) struct Evaluator<'r> {
    pub(super) resolver: &'r dyn NameResolver,
    pub(super) steps: Cell<u32>,
    pub(super) depth: Cell<u32>,
    /// One frame per comprehension being evaluated, innermost last.
    pub(super) locals: RefCell<Vec<HashMap<String, Value>>>,
    /// Address of the `zip`/`reversed`/`enumerate` call being evaluated as a builtin's
    /// argument or a comprehension source; they are one-shot, so refused anywhere else.
    pub(super) direct_iterator: Cell<usize>,
}

impl<'r> Evaluator<'r> {
    pub(crate) fn new(resolver: &'r dyn NameResolver) -> Self {
        Evaluator {
            resolver,
            steps: Cell::new(0),
            depth: Cell::new(0),
            locals: RefCell::new(Vec::new()),
            direct_iterator: Cell::new(0),
        }
    }

    /// Evaluates one top-level expression with a fresh step budget.
    pub(crate) fn eval(&self, expr: &Expr) -> Option<Value> {
        self.steps.set(0);
        self.depth.set(0);
        self.locals.borrow_mut().clear();
        self.ev(expr)
    }

    pub(super) fn tick(&self) -> Option<()> {
        let steps = self.steps.get() + 1;
        self.steps.set(steps);
        (steps <= MAX_STEPS).then_some(())
    }

    fn enter(&self) -> Option<DepthGuard<'_>> {
        let depth = self.depth.get() + 1;
        if depth > MAX_DEPTH {
            return None;
        }
        self.depth.set(depth);
        Some(DepthGuard(&self.depth))
    }

    // Containers and strings over the caps are refused rather than built.
    pub(super) fn capped(value: Value) -> Option<Value> {
        let ok = match &value {
            Value::Str(s) => s.len() <= MAX_STRING,
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => v.len() <= MAX_ELEMENTS,
            Value::Dict(d) => d.len() <= MAX_ELEMENTS,
            _ => true,
        };
        ok.then_some(value)
    }

    pub(super) fn ev(&self, expr: &Expr) -> Option<Value> {
        let _guard = self.enter()?;
        self.tick()?;
        match expr {
            Expr::StringLiteral(s) => Self::capped(Value::Str(s.value.to_str().to_string())),
            Expr::NumberLiteral(n) => match &n.value {
                ast::Number::Int(i) => i.as_i64().map(Value::Int),
                _ => None,
            },
            Expr::BooleanLiteral(b) => Some(Value::Bool(b.value)),
            Expr::NoneLiteral(_) => Some(Value::None),
            Expr::List(l) => {
                Self::capped(Value::List(self.display(&l.elts, DisplayKind::Ordered)?))
            }
            Expr::Tuple(t) => {
                Self::capped(Value::Tuple(self.display(&t.elts, DisplayKind::Ordered)?))
            }
            Expr::Set(s) => {
                let items = self.display(&s.elts, DisplayKind::Unordered)?;
                if !items.iter().all(Value::hashable) {
                    return None;
                }
                let mut set = Vec::new();
                for item in items {
                    set_insert(&mut set, item);
                }
                Self::capped(Value::Set(set))
            }
            Expr::Dict(d) => self.dict_display(d),
            Expr::Name(n) => self.name(n),
            Expr::Attribute(a) => self.ev_attribute(a),
            Expr::FString(f) => self.fstring(f),
            Expr::BinOp(b) => self.binop(&b.left, b.op, &b.right),
            Expr::UnaryOp(u) => {
                let operand = self.ev(&u.operand)?;
                match u.op {
                    UnaryOp::Not => Some(Value::Bool(!operand.truthy())),
                    UnaryOp::USub => operand.number().and_then(i64::checked_neg).map(Value::Int),
                    UnaryOp::UAdd => operand.number().map(Value::Int),
                    UnaryOp::Invert => None,
                }
            }
            Expr::BoolOp(b) => self.ev_bool_op(b),
            Expr::Compare(c) => self.compare(c),
            Expr::If(i) => {
                if self.ev(&i.test)?.truthy() {
                    self.ev(&i.body)
                } else {
                    self.ev(&i.orelse)
                }
            }
            Expr::Subscript(s) => self.subscript(s),
            Expr::Call(c) => self.call(c),
            Expr::ListComp(c) => {
                let mut out = Vec::new();
                self.comprehension(&c.generators, true, &mut || {
                    out.extend(self.comprehension_items(&c.elt, true)?);
                    (out.len() <= MAX_ELEMENTS).then_some(())
                })?;
                Self::capped(Value::List(out))
            }
            Expr::SetComp(c) => {
                let mut out = Vec::new();
                self.comprehension(&c.generators, false, &mut || {
                    for v in self.comprehension_items(&c.elt, false)? {
                        v.hashable().then_some(())?;
                        set_insert(&mut out, v);
                    }
                    (out.len() <= MAX_ELEMENTS).then_some(())
                })?;
                Self::capped(Value::Set(out))
            }
            Expr::DictComp(c) => self.ev_dict_comp(c),
            _ => None,
        }
    }

    fn ev_bool_op(&self, b: &ruff_python_ast::ExprBoolOp) -> Option<Value> {
        let last = b.values.len().checked_sub(1)?;
        for (i, value) in b.values.iter().enumerate() {
            let v = self.ev(value)?;
            let stop = match b.op {
                BoolOp::And => !v.truthy(),
                BoolOp::Or => v.truthy(),
            };
            if stop || i == last {
                return Some(v);
            }
        }
        None
    }

    fn ev_attribute(&self, a: &ruff_python_ast::ExprAttribute) -> Option<Value> {
        if a.ctx != ExprContext::Load {
            return None;
        }
        match self.resolver.resolve_attribute(a) {
            Resolved::Value(v) => Some(v),
            // `Col.HR.value`: the member's value is what the class attribute holds.
            _ if a.attr.as_str() == "value" && matches!(&*a.value, Expr::Attribute(_)) => {
                match self.ev(&a.value)? {
                    v @ Value::Str(_) => Some(v),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn ev_dict_comp(&self, c: &ruff_python_ast::ExprDictComp) -> Option<Value> {
        let mut out = Vec::new();
        self.comprehension(&c.generators, true, &mut || {
            // No key: `{**m for ...}` (PEP 798) merges each mapping in turn.
            let Some(key_expr) = c.key.as_deref() else {
                let Value::Dict(entries) = self.ev(&c.value)? else {
                    return None;
                };
                for (k, v) in entries {
                    dict_insert(&mut out, k, v);
                }
                return (out.len() <= MAX_ELEMENTS).then_some(());
            };
            let k = self.ev(key_expr)?;
            k.hashable().then_some(())?;
            let v = self.ev(&c.value)?;
            dict_insert(&mut out, k, v);
            (out.len() <= MAX_ELEMENTS).then_some(())
        })?;
        Self::capped(Value::Dict(out))
    }

    // One-shot iterators are evaluated only as a builtin's argument or a comprehension
    // source, where the consumer is known to read them once.
    pub(super) fn arg(&self, expr: &Expr, ordered: bool) -> Option<Value> {
        if let Expr::Call(call) = expr {
            let is_iterator = matches!(
                &*call.func,
                Expr::Name(n) if matches!(n.id.as_str(), "zip" | "reversed" | "enumerate")
            );
            if is_iterator {
                self.direct_iterator.set(Self::address(call));
                let value = self.ev(expr);
                self.direct_iterator.set(0);
                return value;
            }
        }
        let Expr::Generator(g) = expr else {
            return self.ev(expr);
        };
        let mut out = Vec::new();
        self.comprehension(&g.generators, ordered, &mut || {
            out.extend(self.comprehension_items(&g.elt, ordered)?);
            (out.len() <= MAX_ELEMENTS).then_some(())
        })?;
        Self::capped(Value::List(out))
    }

    // A comprehension's element, or the items a `*element` (PEP 798) unpacks.
    fn comprehension_items(&self, elt: &Expr, ordered: bool) -> Option<Vec<Value>> {
        let Expr::Starred(starred) = elt else {
            return Some(vec![self.ev(elt)?]);
        };
        let inner = self.ev(&starred.value)?;
        if ordered {
            Self::iterate(&inner)
        } else {
            Self::iterate_unordered(&inner)
        }
    }

    /// The sequence a `for` loop runs over, where a lazy `enumerate`/`zip` is consumed once.
    pub(crate) fn eval_iterable(&self, expr: &Expr) -> Option<Value> {
        self.arg(expr, true)
    }

    pub(super) fn address(call: &ast::ExprCall) -> usize {
        std::ptr::from_ref(call) as usize
    }

    pub(super) fn consumed_directly(&self, call: &ast::ExprCall) -> Option<()> {
        (self.direct_iterator.get() == Self::address(call)).then_some(())
    }

    pub(super) fn name(&self, name: &ast::ExprName) -> Option<Value> {
        if name.ctx != ExprContext::Load {
            return None;
        }
        for frame in self.locals.borrow().iter().rev() {
            if let Some(v) = frame.get(name.id.as_str()) {
                return Some(v.clone());
            }
        }
        match self.resolver.resolve(name) {
            Resolved::Value(v) => Some(v),
            Resolved::Builtin | Resolved::Unknown => None,
        }
    }

    // Ordered iteration. A set is refused: Python's order for it depends on hash
    // seeds, so the result would be a guess.
    pub(crate) fn iterate(value: &Value) -> Option<Vec<Value>> {
        match value {
            Value::List(v) | Value::Tuple(v) => Some(v.clone()),
            Value::Dict(d) => Some(d.iter().map(|(k, _)| k.clone()).collect()),
            Value::Str(s) => Some(s.chars().map(|c| Value::Str(c.to_string())).collect()),
            _ => None,
        }
    }

    pub(super) fn iterate_unordered(value: &Value) -> Option<Vec<Value>> {
        match value {
            Value::Set(v) => Some(v.clone()),
            other => Self::iterate(other),
        }
    }

    pub(super) fn display(&self, elts: &[Expr], kind: DisplayKind) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        for elt in elts {
            if let Expr::Starred(s) = elt {
                let inner = self.ev(&s.value)?;
                let items = match kind {
                    DisplayKind::Ordered => Self::iterate(&inner)?,
                    DisplayKind::Unordered => Self::iterate_unordered(&inner)?,
                };
                out.extend(items);
            } else {
                out.push(self.ev(elt)?);
            }
            if out.len() > MAX_ELEMENTS {
                return None;
            }
        }
        Some(out)
    }

    fn dict_display(&self, dict: &ast::ExprDict) -> Option<Value> {
        let mut out: Vec<(Value, Value)> = Vec::new();
        for item in &dict.items {
            match &item.key {
                Some(key) => {
                    let k = self.ev(key)?;
                    k.hashable().then_some(())?;
                    dict_insert(&mut out, k, self.ev(&item.value)?);
                }
                None => match self.ev(&item.value)? {
                    Value::Dict(spread) => {
                        for (k, v) in spread {
                            dict_insert(&mut out, k, v);
                        }
                    }
                    _ => return None,
                },
            }
        }
        Self::capped(Value::Dict(out))
    }

    fn fstring(&self, fstring: &ast::ExprFString) -> Option<Value> {
        let mut out = String::new();
        for part in fstring.value.as_slice() {
            self.fstring_part(&mut out, part)?;
        }
        Some(Value::Str(out))
    }

    fn fstring_part(&self, out: &mut String, part: &ruff_python_ast::FStringPart) -> Option<()> {
        match part {
            ast::FStringPart::Literal(lit) => out.push_str(&lit.value),
            ast::FStringPart::FString(f) => {
                for element in f.elements.iter() {
                    match element {
                        ast::InterpolatedStringElement::Literal(lit) => {
                            out.push_str(&lit.value);
                        }
                        ast::InterpolatedStringElement::Interpolation(interp) => {
                            self.fstring_interpolation(out, interp)?;
                        }
                    }
                }
            }
        }
        let _: () = if out.len() > MAX_STRING {
            return None;
        };
        Some(())
    }

    fn fstring_interpolation(
        &self,
        out: &mut String,
        interp: &ruff_python_ast::InterpolatedElement,
    ) -> Option<()> {
        if interp.conversion != ast::ConversionFlag::None
            || interp.format_spec.is_some()
            || interp.debug_text.is_some()
        {
            return None;
        }
        out.push_str(&scalar_text(&self.ev(&interp.expression)?)?);
        Some(())
    }
}
