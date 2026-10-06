//! Static evaluation of the values column lists, dicts and strings are built from.
//!
//! "Statically resolvable" means the value does not depend on IO, randomness, time, the
//! environment, function arguments, or any other runtime state. This module evaluates
//! such expressions (`Evaluator`), works out which names refer to one fixed value
//! (`Analysis`), and folds the results back into the AST as literals (`fold_constants`)
//! so every consumer in the checker sees a literal instead of a variable.

use crate::constants::{LOAD_FUNCTIONS, LOAD_MODULES};
use ruff_python_ast::visitor::transformer::{self as ast_transformer, Transformer};
use ruff_python_ast::visitor::{self as ast_visitor, Visitor};
use ruff_python_ast::{
    self as ast, BoolOp, CmpOp, Expr, ExprContext, ModModule, Operator, UnaryOp,
};
use ruff_text_size::{Ranged, TextRange, TextSize};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MAX_ELEMENTS: usize = 10_000;
const MAX_STEPS: u32 = 50_000;
const MAX_DEPTH: u32 = 64;
const MAX_STRING: usize = 100_000;

/// A value known without running the program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    None,
    List(Vec<Value>),
    Tuple(Vec<Value>),
    /// Insertion-ordered here, but Python's order is hash-dependent, so nothing may turn
    /// a set into an ordered sequence (see `Evaluator::iterate`).
    Set(Vec<Value>),
    Dict(Vec<(Value, Value)>),
}

impl Value {
    /// The strings of a list/tuple/set of strings -- a column list.
    #[cfg(test)]
    pub(crate) fn as_string_list(&self) -> Option<Vec<String>> {
        let items = match self {
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => v,
            _ => return None,
        };
        items
            .iter()
            .map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// Nothing reachable from the value can be changed: a scalar, or a tuple of such. It
    /// can be aliased, passed anywhere and returned without risk.
    pub(crate) fn is_immutable(&self) -> bool {
        match self {
            Value::Str(_) | Value::Int(_) | Value::Bool(_) | Value::None => true,
            Value::Tuple(items) => items.iter().all(Value::is_immutable),
            _ => false,
        }
    }

    /// Every element is an immutable scalar (or a tuple of them), so handing out a
    /// reference to an element can't let anyone change the container's contents.
    pub(crate) fn has_immutable_elements(&self) -> bool {
        fn scalar(v: &Value) -> bool {
            match v {
                Value::Str(_) | Value::Int(_) | Value::Bool(_) | Value::None => true,
                Value::Tuple(items) => items.iter().all(scalar),
                _ => false,
            }
        }
        match self {
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => v.iter().all(scalar),
            Value::Dict(items) => items.iter().all(|(k, v)| scalar(k) && scalar(v)),
            _ => true,
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Value::Str(s) => !s.is_empty(),
            Value::Int(i) => *i != 0,
            Value::Bool(b) => *b,
            Value::None => false,
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => !v.is_empty(),
            Value::Dict(d) => !d.is_empty(),
        }
    }

    fn hashable(&self) -> bool {
        match self {
            Value::Str(_) | Value::Int(_) | Value::Bool(_) | Value::None => true,
            Value::Tuple(items) => items.iter().all(Value::hashable),
            _ => false,
        }
    }

    // `True == 1` in Python, so numbers compare across `Int`/`Bool`.
    fn number(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    fn py_eq(&self, other: &Value) -> bool {
        if let (Some(a), Some(b)) = (self.number(), other.number()) {
            return a == b;
        }
        match (self, other) {
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::None, Value::None) => true,
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.py_eq(y))
            }
            (Value::Set(a), Value::Set(b)) => {
                a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| x.py_eq(y)))
            }
            (Value::Dict(a), Value::Dict(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, v)| {
                        b.iter()
                            .find(|(k2, _)| k.py_eq(k2))
                            .is_some_and(|(_, v2)| v.py_eq(v2))
                    })
            }
            _ => false,
        }
    }

    fn compare(&self, other: &Value) -> Option<Ordering> {
        if let (Some(a), Some(b)) = (self.number(), other.number()) {
            return Some(a.cmp(&b));
        }
        match (self, other) {
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
                for (x, y) in a.iter().zip(b) {
                    match x.compare(y)? {
                        Ordering::Equal => {}
                        other => return Some(other),
                    }
                }
                Some(a.len().cmp(&b.len()))
            }
            _ => None,
        }
    }
}

fn set_insert(items: &mut Vec<Value>, value: Value) {
    if !items.iter().any(|v| v.py_eq(&value)) {
        items.push(value);
    }
}

fn dict_insert(items: &mut Vec<(Value, Value)>, key: Value, value: Value) {
    match items.iter_mut().find(|(k, _)| k.py_eq(&key)) {
        Some(entry) => entry.1 = value,
        None => items.push((key, value)),
    }
}

/// What the evaluator learns about a name from its surroundings.
pub(crate) enum Resolved {
    Value(Value),
    /// Not bound anywhere in the file, so it is a builtin (or undefined).
    Builtin,
    Unknown,
}

pub(crate) trait NameResolver {
    fn resolve(&self, name: &ast::ExprName) -> Resolved;

    /// `Config.COLUMNS`: an attribute of a class defined in the file.
    fn resolve_attribute(&self, _attribute: &ast::ExprAttribute) -> Resolved {
        Resolved::Unknown
    }

    /// `cfg.cols` where `cfg` is an imported project module: true when the attribute is a
    /// member of that module (a constant or a function).
    fn is_module_member(&self, _attribute: &ast::ExprAttribute) -> bool {
        false
    }

    /// `cfg.cols(...)`: a call to a function of an imported project module.
    fn call_module_function(
        &self,
        _attribute: &ast::ExprAttribute,
        _args: Vec<Value>,
        _kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        None
    }

    /// A call to a function defined in the file whose body can be evaluated. `None` when
    /// the callee isn't one, or the call can't be evaluated.
    fn call_function(
        &self,
        _callee: &ast::ExprName,
        _args: Vec<Value>,
        _kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        None
    }
}

struct DepthGuard<'e>(&'e Cell<u32>);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

pub(crate) struct Evaluator<'r> {
    resolver: &'r dyn NameResolver,
    steps: Cell<u32>,
    depth: Cell<u32>,
    /// One frame per comprehension being evaluated, innermost last.
    locals: RefCell<Vec<HashMap<String, Value>>>,
    /// Address of the `zip`/`reversed`/`enumerate` call being evaluated as a builtin's
    /// argument or a comprehension source; they are one-shot, so refused anywhere else.
    direct_iterator: Cell<usize>,
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

    fn tick(&self) -> Option<()> {
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
    fn capped(value: Value) -> Option<Value> {
        let ok = match &value {
            Value::Str(s) => s.len() <= MAX_STRING,
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => v.len() <= MAX_ELEMENTS,
            Value::Dict(d) => d.len() <= MAX_ELEMENTS,
            _ => true,
        };
        ok.then_some(value)
    }

    fn ev(&self, expr: &Expr) -> Option<Value> {
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
            Expr::Attribute(a) => {
                if a.ctx != ExprContext::Load {
                    return None;
                }
                match self.resolver.resolve_attribute(a) {
                    Resolved::Value(v) => Some(v),
                    _ => None,
                }
            }
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
            Expr::BoolOp(b) => {
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
                    out.push(self.ev(&c.elt)?);
                    (out.len() <= MAX_ELEMENTS).then_some(())
                })?;
                Self::capped(Value::List(out))
            }
            Expr::SetComp(c) => {
                let mut out = Vec::new();
                self.comprehension(&c.generators, false, &mut || {
                    let v = self.ev(&c.elt)?;
                    v.hashable().then_some(())?;
                    set_insert(&mut out, v);
                    (out.len() <= MAX_ELEMENTS).then_some(())
                })?;
                Self::capped(Value::Set(out))
            }
            Expr::DictComp(c) => {
                let key_expr = c.key.as_deref()?;
                let mut out = Vec::new();
                self.comprehension(&c.generators, true, &mut || {
                    let k = self.ev(key_expr)?;
                    k.hashable().then_some(())?;
                    let v = self.ev(&c.value)?;
                    dict_insert(&mut out, k, v);
                    (out.len() <= MAX_ELEMENTS).then_some(())
                })?;
                Self::capped(Value::Dict(out))
            }
            _ => None,
        }
    }

    // One-shot iterators are evaluated only as a builtin's argument or a comprehension
    // source, where the consumer is known to read them once.
    fn arg(&self, expr: &Expr, ordered: bool) -> Option<Value> {
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
            out.push(self.ev(&g.elt)?);
            (out.len() <= MAX_ELEMENTS).then_some(())
        })?;
        Self::capped(Value::List(out))
    }

    fn address(call: &ast::ExprCall) -> usize {
        std::ptr::from_ref(call) as usize
    }

    fn consumed_directly(&self, call: &ast::ExprCall) -> Option<()> {
        (self.direct_iterator.get() == Self::address(call)).then_some(())
    }

    fn name(&self, name: &ast::ExprName) -> Option<Value> {
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
    fn iterate(value: &Value) -> Option<Vec<Value>> {
        match value {
            Value::List(v) | Value::Tuple(v) => Some(v.clone()),
            Value::Dict(d) => Some(d.iter().map(|(k, _)| k.clone()).collect()),
            Value::Str(s) => Some(s.chars().map(|c| Value::Str(c.to_string())).collect()),
            _ => None,
        }
    }

    fn iterate_unordered(value: &Value) -> Option<Vec<Value>> {
        match value {
            Value::Set(v) => Some(v.clone()),
            other => Self::iterate(other),
        }
    }

    fn display(&self, elts: &[Expr], kind: DisplayKind) -> Option<Vec<Value>> {
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
            match part {
                ast::FStringPart::Literal(lit) => out.push_str(&lit.value),
                ast::FStringPart::FString(f) => {
                    for element in f.elements.iter() {
                        match element {
                            ast::InterpolatedStringElement::Literal(lit) => {
                                out.push_str(&lit.value);
                            }
                            ast::InterpolatedStringElement::Interpolation(interp) => {
                                if interp.conversion != ast::ConversionFlag::None
                                    || interp.format_spec.is_some()
                                    || interp.debug_text.is_some()
                                {
                                    return None;
                                }
                                out.push_str(&scalar_text(&self.ev(&interp.expression)?)?);
                            }
                        }
                    }
                }
            }
            if out.len() > MAX_STRING {
                return None;
            }
        }
        Some(Value::Str(out))
    }

    fn binop(&self, left: &Expr, op: Operator, right: &Expr) -> Option<Value> {
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

    fn compare(&self, cmp: &ast::ExprCompare) -> Option<Value> {
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

    fn subscript(&self, sub: &ast::ExprSubscript) -> Option<Value> {
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

    fn comprehension(
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

    fn bind(&self, target: &Expr, value: Value) -> Option<()> {
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

    fn call(&self, call: &ast::ExprCall) -> Option<Value> {
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
    fn positional(&self, call: &ast::ExprCall, ordered: bool) -> Option<Vec<Value>> {
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
    fn positional_exact(&self, call: &ast::ExprCall) -> Option<Vec<Value>> {
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

    fn keyword(&self, call: &ast::ExprCall, name: &str) -> Option<Option<Value>> {
        let mut found = None;
        for kw in call.arguments.keywords.iter() {
            if kw.arg.as_ref().map(|a| a.as_str()) == Some(name) {
                found = Some(self.ev(&kw.value)?);
            }
        }
        Some(found)
    }

    fn only_keywords(call: &ast::ExprCall, allowed: &[&str]) -> bool {
        call.arguments.keywords.iter().all(|k| {
            k.arg
                .as_ref()
                .is_some_and(|a| allowed.contains(&a.as_str()))
        })
    }

    fn builtin(&self, name: &str, call: &ast::ExprCall) -> Option<Value> {
        let ordered = !matches!(
            name,
            "set" | "frozenset" | "sorted" | "min" | "max" | "sum" | "any" | "all"
        );
        match name {
            "len" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional_exact(call)?;
                let [value] = args.as_slice() else {
                    return None;
                };
                let n = match value {
                    Value::Str(s) => s.chars().count(),
                    Value::List(v) | Value::Tuple(v) | Value::Set(v) => v.len(),
                    Value::Dict(d) => d.len(),
                    _ => return None,
                };
                i64::try_from(n).ok().map(Value::Int)
            }
            "list" | "tuple" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional(call, ordered)?;
                let items = match args.as_slice() {
                    [] => Vec::new(),
                    [value] => Self::iterate(value)?,
                    _ => return None,
                };
                Self::capped(if name == "list" {
                    Value::List(items)
                } else {
                    Value::Tuple(items)
                })
            }
            "set" | "frozenset" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional(call, false)?;
                let items = match args.as_slice() {
                    [] => Vec::new(),
                    [value] => Self::iterate_unordered(value)?,
                    _ => return None,
                };
                items.iter().all(Value::hashable).then_some(())?;
                let mut set = Vec::new();
                for item in items {
                    set_insert(&mut set, item);
                }
                Self::capped(Value::Set(set))
            }
            "sorted" => {
                Self::only_keywords(call, &["reverse"]).then_some(())?;
                let reverse = match self.keyword(call, "reverse")? {
                    None => false,
                    Some(Value::Bool(b)) => b,
                    Some(_) => return None,
                };
                let args = self.positional(call, false)?;
                let [value] = args.as_slice() else {
                    return None;
                };
                let mut items = Self::iterate_unordered(value)?;
                // Python raises on values that can't be ordered; refuse rather than guess.
                let mut comparable = true;
                items.sort_by(|a, b| {
                    a.compare(b).unwrap_or_else(|| {
                        comparable = false;
                        Ordering::Equal
                    })
                });
                comparable.then_some(())?;
                if reverse {
                    items.reverse();
                }
                Self::capped(Value::List(items))
            }
            "reversed" => {
                self.consumed_directly(call)?;
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional_exact(call)?;
                let [value] = args.as_slice() else {
                    return None;
                };
                let mut items = Self::iterate(value)?;
                items.reverse();
                Self::capped(Value::List(items))
            }
            "range" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional_exact(call)?;
                let numbers: Option<Vec<i64>> = args.iter().map(Value::number).collect();
                let (start, stop, step) = match numbers?.as_slice() {
                    [stop] => (0, *stop, 1),
                    [start, stop] => (*start, *stop, 1),
                    [start, stop, step] => (*start, *stop, *step),
                    _ => return None,
                };
                (step != 0).then_some(())?;
                let mut out = Vec::new();
                let mut current = start;
                while (step > 0 && current < stop) || (step < 0 && current > stop) {
                    out.push(Value::Int(current));
                    (out.len() <= MAX_ELEMENTS).then_some(())?;
                    current = current.checked_add(step)?;
                }
                Some(Value::List(out))
            }
            "zip" => {
                self.consumed_directly(call)?;
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional(call, true)?;
                let columns: Option<Vec<Vec<Value>>> = args.iter().map(Self::iterate).collect();
                let columns = columns?;
                let rows = columns.iter().map(Vec::len).min().unwrap_or(0);
                let out = (0..rows)
                    .map(|i| Value::Tuple(columns.iter().map(|c| c[i].clone()).collect()))
                    .collect();
                Self::capped(Value::List(out))
            }
            "enumerate" => {
                self.consumed_directly(call)?;
                Self::only_keywords(call, &["start"]).then_some(())?;
                let args = self.positional(call, true)?;
                let start = match self.keyword(call, "start")? {
                    Some(v) => v.number()?,
                    None => 0,
                };
                let [value] = args.as_slice() else {
                    return None;
                };
                let items = Self::iterate(value)?;
                let mut out = Vec::new();
                for (i, item) in items.into_iter().enumerate() {
                    out.push(Value::Tuple(vec![
                        Value::Int(start.checked_add(i64::try_from(i).ok()?)?),
                        item,
                    ]));
                }
                Self::capped(Value::List(out))
            }
            "min" | "max" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional(call, false)?;
                let items = match args.as_slice() {
                    [single] => Self::iterate_unordered(single)?,
                    many if many.len() >= 2 => many.to_vec(),
                    _ => return None,
                };
                let mut best = items.first()?.clone();
                for item in &items[1..] {
                    let order = item.compare(&best)?;
                    if (name == "min" && order == Ordering::Less)
                        || (name == "max" && order == Ordering::Greater)
                    {
                        best = item.clone();
                    }
                }
                Some(best)
            }
            "sum" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional(call, false)?;
                let (value, start) = match args.as_slice() {
                    [v] => (v, 0),
                    [v, s] => (v, s.number()?),
                    _ => return None,
                };
                let mut total = start;
                for item in Self::iterate_unordered(value)? {
                    total = total.checked_add(item.number()?)?;
                }
                Some(Value::Int(total))
            }
            "dict" => {
                let mut out: Vec<(Value, Value)> = Vec::new();
                let args = self.positional(call, true)?;
                match args.as_slice() {
                    [] => {}
                    [Value::Dict(d)] => out = d.clone(),
                    [other] => {
                        for pair in Self::iterate(other)? {
                            let (Value::List(p) | Value::Tuple(p)) = pair else {
                                return None;
                            };
                            let [k, v] = p.as_slice() else {
                                return None;
                            };
                            k.hashable().then_some(())?;
                            dict_insert(&mut out, k.clone(), v.clone());
                        }
                    }
                    _ => return None,
                }
                for kw in call.arguments.keywords.iter() {
                    let key = kw.arg.as_ref()?;
                    dict_insert(&mut out, Value::Str(key.to_string()), self.ev(&kw.value)?);
                }
                Self::capped(Value::Dict(out))
            }
            "str" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional_exact(call)?;
                let [value] = args.as_slice() else {
                    return None;
                };
                scalar_text(value).map(Value::Str)
            }
            "int" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional_exact(call)?;
                match args.as_slice() {
                    [Value::Str(s)] => s.trim().parse::<i64>().ok().map(Value::Int),
                    [v] => v.number().map(Value::Int),
                    _ => None,
                }
            }
            "bool" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional_exact(call)?;
                match args.as_slice() {
                    [] => Some(Value::Bool(false)),
                    [v] => Some(Value::Bool(v.truthy())),
                    _ => None,
                }
            }
            "any" | "all" => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional(call, false)?;
                let [value] = args.as_slice() else {
                    return None;
                };
                let items = Self::iterate_unordered(value)?;
                Some(Value::Bool(if name == "any" {
                    items.iter().any(Value::truthy)
                } else {
                    items.iter().all(Value::truthy)
                }))
            }
            _ => None,
        }
    }

    fn method(&self, receiver: Value, name: &str, call: &ast::ExprCall) -> Option<Value> {
        match receiver {
            Value::Str(s) => self.str_method(&s, name, call),
            Value::Dict(d) => {
                Self::only_keywords(call, &[]).then_some(())?;
                let args = self.positional_exact(call)?;
                match (name, args.as_slice()) {
                    ("keys", []) => Some(Value::List(d.iter().map(|(k, _)| k.clone()).collect())),
                    ("values", []) => Some(Value::List(d.iter().map(|(_, v)| v.clone()).collect())),
                    ("items", []) => Some(Value::List(
                        d.iter()
                            .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                            .collect(),
                    )),
                    ("copy", []) => Some(Value::Dict(d)),
                    ("get", [key]) | ("get", [key, _]) => {
                        let default = args.get(1).cloned().unwrap_or(Value::None);
                        Some(
                            d.iter()
                                .find(|(k, _)| k.py_eq(key))
                                .map_or(default, |(_, v)| v.clone()),
                        )
                    }
                    _ => None,
                }
            }
            Value::List(v) if name == "copy" => {
                Self::only_keywords(call, &[]).then_some(())?;
                call.arguments.args.is_empty().then_some(Value::List(v))
            }
            _ => None,
        }
    }

    fn str_method(&self, s: &str, name: &str, call: &ast::ExprCall) -> Option<Value> {
        if name == "format" {
            return self.str_format(s, call);
        }
        Self::only_keywords(call, &[]).then_some(())?;
        let args = self.positional(call, true)?;
        let text = |v: &Value| match v {
            Value::Str(t) => Some(t.clone()),
            _ => None,
        };
        match (name, args.as_slice()) {
            ("upper", []) => Some(Value::Str(s.to_uppercase())),
            ("lower", []) => Some(Value::Str(s.to_lowercase())),
            ("capitalize", []) => {
                let mut chars = s.chars();
                let first = chars.next()?;
                Some(Value::Str(format!(
                    "{}{}",
                    first.to_uppercase(),
                    chars.as_str().to_lowercase()
                )))
            }
            ("title", []) => {
                let mut out = String::new();
                let mut in_word = false;
                for c in s.chars() {
                    if c.is_lowercase() || c.is_uppercase() {
                        // Titlecase digraphs and ß map to more than one character.
                        (c.to_uppercase().count() == 1 && c.to_lowercase().count() == 1)
                            .then_some(())?;
                        if in_word {
                            out.extend(c.to_lowercase());
                        } else {
                            out.extend(c.to_uppercase());
                        }
                        in_word = true;
                    } else {
                        in_word = false;
                        out.push(c);
                    }
                }
                Some(Value::Str(out))
            }
            ("strip", []) => Some(Value::Str(s.trim().to_string())),
            ("lstrip", []) => Some(Value::Str(s.trim_start().to_string())),
            ("rstrip", []) => Some(Value::Str(s.trim_end().to_string())),
            ("strip", [chars]) => {
                let set: Vec<char> = text(chars)?.chars().collect();
                Some(Value::Str(s.trim_matches(|c| set.contains(&c)).to_string()))
            }
            ("lstrip", [chars]) => {
                let set: Vec<char> = text(chars)?.chars().collect();
                Some(Value::Str(
                    s.trim_start_matches(|c| set.contains(&c)).to_string(),
                ))
            }
            ("rstrip", [chars]) => {
                let set: Vec<char> = text(chars)?.chars().collect();
                Some(Value::Str(
                    s.trim_end_matches(|c| set.contains(&c)).to_string(),
                ))
            }
            ("replace", [old, new]) => {
                let (old, new) = (text(old)?, text(new)?);
                (!old.is_empty()).then_some(())?;
                Self::capped(Value::Str(s.replace(&old, &new)))
            }
            ("split", []) => Some(Value::List(
                s.split_whitespace()
                    .map(|p| Value::Str(p.to_string()))
                    .collect(),
            )),
            ("split", [sep]) => {
                let sep = text(sep)?;
                (!sep.is_empty()).then_some(())?;
                Some(Value::List(
                    s.split(sep.as_str())
                        .map(|p| Value::Str(p.to_string()))
                        .collect(),
                ))
            }
            ("join", [iterable]) => {
                let parts: Option<Vec<String>> =
                    Self::iterate(iterable)?.iter().map(text).collect();
                Self::capped(Value::Str(parts?.join(s)))
            }
            ("startswith", [prefix]) | ("endswith", [prefix]) => {
                let candidates: Vec<String> = match prefix {
                    Value::Str(p) => vec![p.clone()],
                    Value::Tuple(items) => items.iter().map(text).collect::<Option<_>>()?,
                    _ => return None,
                };
                let hit = candidates.iter().any(|c| {
                    if name == "startswith" {
                        s.starts_with(c.as_str())
                    } else {
                        s.ends_with(c.as_str())
                    }
                });
                Some(Value::Bool(hit))
            }
            ("removeprefix", [p]) => {
                let p = text(p)?;
                Some(Value::Str(
                    s.strip_prefix(p.as_str()).unwrap_or(s).to_string(),
                ))
            }
            ("removesuffix", [p]) => {
                let p = text(p)?;
                Some(Value::Str(
                    s.strip_suffix(p.as_str()).unwrap_or(s).to_string(),
                ))
            }
            _ => None,
        }
    }

    // `"{}_{name}".format(...)`: positional, auto-numbered and keyword fields only, no
    // conversions, format specs, or attribute/index access.
    fn str_format(&self, template: &str, call: &ast::ExprCall) -> Option<Value> {
        let positional = self.positional_exact(call)?;
        let mut named: HashMap<String, Value> = HashMap::new();
        for kw in call.arguments.keywords.iter() {
            named.insert(kw.arg.as_ref()?.to_string(), self.ev(&kw.value)?);
        }
        let mut out = String::new();
        let mut chars = template.chars().peekable();
        let mut next_auto = 0usize;
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    out.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    out.push('}');
                }
                '{' => {
                    let mut field = String::new();
                    loop {
                        match chars.next()? {
                            '}' => break,
                            c => field.push(c),
                        }
                    }
                    let value = if field.is_empty() {
                        let v = positional.get(next_auto)?;
                        next_auto += 1;
                        v
                    } else if let Ok(i) = field.parse::<usize>() {
                        positional.get(i)?
                    } else if field.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        named.get(&field)?
                    } else {
                        return None;
                    };
                    out.push_str(&scalar_text(value)?);
                }
                '}' => return None,
                c => out.push(c),
            }
            (out.len() <= MAX_STRING).then_some(())?;
        }
        Some(Value::Str(out))
    }
}

#[derive(Clone, Copy)]
enum DisplayKind {
    Ordered,
    Unordered,
}

// How `str()`/an f-string interpolation prints a scalar. Containers are not handled:
// their repr has quoting rules that are easy to get subtly wrong.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Bool(true) => Some("True".to_string()),
        Value::Bool(false) => Some("False".to_string()),
        Value::None => Some("None".to_string()),
        _ => None,
    }
}

fn contains(container: &Value, item: &Value) -> Option<bool> {
    match container {
        Value::List(v) | Value::Tuple(v) | Value::Set(v) => Some(v.iter().any(|x| x.py_eq(item))),
        Value::Dict(d) => Some(d.iter().any(|(k, _)| k.py_eq(item))),
        Value::Str(s) => match item {
            Value::Str(t) => Some(s.contains(t.as_str())),
            _ => None,
        },
        _ => None,
    }
}

fn none_identity(left: &Value, right: &Value) -> Option<bool> {
    match (left, right) {
        (Value::None, Value::None) => Some(true),
        (Value::None, _) | (_, Value::None) => Some(false),
        _ => None,
    }
}

fn normalize_index(index: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).ok()?;
    let resolved = if index < 0 { index + len } else { index };
    (0..len)
        .contains(&resolved)
        .then(|| usize::try_from(resolved).ok())?
}

// How a sliced sequence's picked elements become a value of the same kind again.
type Rebuild = fn(Vec<Value>) -> Value;

fn slice_value(
    base: &Value,
    lower: Option<i64>,
    upper: Option<i64>,
    step: Option<i64>,
) -> Option<Value> {
    let step = step.unwrap_or(1);
    (step != 0).then_some(())?;
    let (items, rebuild): (Vec<Value>, Rebuild) = match base {
        Value::List(v) => (v.clone(), Value::List),
        Value::Tuple(v) => (v.clone(), Value::Tuple),
        Value::Str(s) => {
            let chars: Vec<Value> = s.chars().map(|c| Value::Str(c.to_string())).collect();
            let joined = |parts: Vec<Value>| {
                Value::Str(
                    parts
                        .into_iter()
                        .filter_map(|p| match p {
                            Value::Str(s) => Some(s),
                            _ => None,
                        })
                        .collect(),
                )
            };
            (chars, joined)
        }
        _ => return None,
    };
    let len = i64::try_from(items.len()).ok()?;
    let clamp = |i: i64, lo: i64, hi: i64| i.clamp(lo, hi);
    let mut picked = Vec::new();
    if step > 0 {
        let start = lower.map_or(0, |i| clamp(if i < 0 { i + len } else { i }, 0, len));
        let stop = upper.map_or(len, |i| clamp(if i < 0 { i + len } else { i }, 0, len));
        let mut i = start;
        while i < stop {
            picked.push(items[usize::try_from(i).ok()?].clone());
            let Some(next) = i.checked_add(step) else {
                break;
            };
            i = next;
        }
    } else {
        let start = lower.map_or(len - 1, |i| {
            clamp(if i < 0 { i + len } else { i }, -1, len - 1)
        });
        let stop = upper.map_or(-1, |i| clamp(if i < 0 { i + len } else { i }, -1, len - 1));
        let mut i = start;
        while i > stop {
            picked.push(items[usize::try_from(i).ok()?].clone());
            let Some(next) = i.checked_add(step) else {
                break;
            };
            i = next;
        }
    }
    Some(rebuild(picked))
}

// ───────────────────────────── scope analysis ─────────────────────────────

/// What a consumer of an expression's value can do with the objects it refers to.
/// Ordered by how much it endangers a name's value.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Use {
    /// Cannot retain or change anything reachable from the value (`len(x)`, `x in y`,
    /// an f-string interpolation, an argument of a loader).
    Safe,
    /// May hand out references to the value's elements (iteration, indexing, `+`,
    /// `list(x)`): harmless only if the elements are immutable.
    Interior,
    /// May alias or change the value: passed to an unknown function, assigned to
    /// another name, returned, stored, mutated in place.
    Escapes,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScopeKind {
    Module,
    Function,
    Class,
    Comprehension,
}

enum Binding<'a> {
    /// `name = <expr>` or `name: T = <expr>` -- the form a value is read from.
    Assign(&'a Expr),
    /// `def name(...)`: callable if its body can be evaluated.
    Function(&'a ast::StmtFunctionDef),
    /// `class name: ...`, with the id of the scope holding its body.
    Class(&'a ast::StmtClassDef, usize),
    /// `from module import name [as alias]` (leading dots as `level`).
    ImportFrom(u32, String, String),
    /// `import module [as alias]`.
    ImportModule(String),
    /// Any other way of binding a name (augmented assignment, unpacking, `for`/`with`/
    /// `except` target, import, `def`, `class`, walrus, parameter, `del`, ...).
    Other,
}

struct Scope<'a> {
    kind: ScopeKind,
    parent: Option<usize>,
    bindings: HashMap<String, Vec<Binding<'a>>>,
    globals: HashSet<String>,
    nonlocals: HashSet<String>,
}

struct Occurrence {
    offset: TextSize,
    scope: usize,
    name: String,
    use_: Use,
    // The classification holds only while this builtin is not rebound anywhere.
    builtin: Option<&'static str>,
}

type GroupKey = (usize, String);

// Builtins whose arguments they only inspect.
const SAFE_BUILTINS: &[&str] = &[
    "len",
    "print",
    "any",
    "all",
    "bool",
    "str",
    "repr",
    "type",
    "isinstance",
    "id",
    "callable",
    "int",
    "hash",
];
// Builtins that build something out of their arguments' elements.
const INTERIOR_BUILTINS: &[&str] = &[
    "list",
    "tuple",
    "set",
    "frozenset",
    "sorted",
    "reversed",
    "dict",
    "zip",
    "enumerate",
    "iter",
    "next",
    "min",
    "max",
    "sum",
    "range",
    "filter",
    "map",
];
// Methods that neither change their receiver nor return part of it.
const SAFE_METHODS: &[&str] = &[
    "index",
    "count",
    "keys",
    "startswith",
    "endswith",
    "upper",
    "lower",
    "title",
    "capitalize",
    "strip",
    "lstrip",
    "rstrip",
    "replace",
    "split",
    "rsplit",
    "format",
    "find",
    "rfind",
    "isdigit",
    "isalpha",
    "isalnum",
    "isupper",
    "islower",
    "removeprefix",
    "removesuffix",
    "zfill",
    "ljust",
    "rjust",
    "center",
    "casefold",
    "splitlines",
];
// Methods that return part of their receiver (or a shallow copy of it).
const INTERIOR_METHODS: &[&str] = &["values", "items", "get", "copy"];
// DataFrame/Series API methods: they read the column lists, mappings and keys they are
// given and neither change nor keep them, which is what makes `df.select(KEEP)` or
// `df.rename(columns=MAPPING)` safe for a constant to be passed to. Deliberately NOT
// here: anything that is also a container mutator or stores its argument (`append`,
// `extend`, `insert`, `update`, `add`, `setdefault`, ...).
const FRAME_METHODS: &[&str] = &[
    "select",
    "drop",
    "rename",
    "merge",
    "groupby",
    "group_by",
    "sort_values",
    "sort",
    "filter",
    "with_columns",
    "with_column",
    "assign",
    "pivot",
    "pivot_table",
    "melt",
    "unpivot",
    "explode",
    "reindex",
    "set_index",
    "reset_index",
    "agg",
    "aggregate",
    "apply",
    "map",
    "applymap",
    "transform",
    "astype",
    "cast",
    "fillna",
    "fill_null",
    "fill_nan",
    "dropna",
    "drop_nulls",
    "drop_duplicates",
    "unique",
    "isin",
    "is_in",
    "query",
    "rolling",
    "resample",
    "shift",
    "diff",
    "pct_change",
    "nlargest",
    "nsmallest",
    "value_counts",
    "describe",
    "head",
    "tail",
    "sample",
    "to_dict",
    "to_csv",
    "to_parquet",
    "to_numpy",
    "to_list",
    "to_pandas",
    "to_polars",
    "over",
    "alias",
    "slice",
    "gather",
    "take",
    "cumsum",
    "mean",
    "sum",
    "min",
    "max",
    "count",
    "std",
    "var",
    "median",
    "first",
    "last",
    "nunique",
    "duplicated",
    "rank",
    "clip",
    "round",
    "abs",
    "where",
    "mask",
    "stack",
    "unstack",
    "transpose",
    "reorder_levels",
    "swaplevel",
    "xs",
    "between",
    "contains",
    "startswith",
    "endswith",
    "lower",
    "upper",
    "strip",
    "split",
    "cat",
    "get_dummies",
    "concat",
    "lazy",
    "collect",
    "scan",
    "sink_csv",
    "sink_parquet",
    "write_csv",
    "write_parquet",
    "write_json",
];

// The only names an annotation may use for its value to still be what the name holds.
const PLAIN_ANNOTATION_NAMES: &[&str] = &[
    "list",
    "List",
    "tuple",
    "Tuple",
    "dict",
    "Dict",
    "set",
    "Set",
    "frozenset",
    "FrozenSet",
    "str",
    "int",
    "bool",
    "float",
    "bytes",
    "None",
    "Final",
    "ClassVar",
    "Sequence",
    "Mapping",
    "Iterable",
    "Collection",
    "MutableSequence",
    "MutableMapping",
    "Any",
    "Optional",
    "Union",
    "typing",
    "collections",
    "abc",
];

fn annotation_is_plain(annotation: &Expr) -> bool {
    match annotation {
        Expr::Name(n) => PLAIN_ANNOTATION_NAMES.contains(&n.id.as_str()),
        Expr::Attribute(a) => {
            PLAIN_ANNOTATION_NAMES.contains(&a.attr.as_str()) && annotation_is_plain(&a.value)
        }
        Expr::Subscript(s) => annotation_is_plain(&s.value) && annotation_is_plain(&s.slice),
        Expr::Tuple(t) => t.elts.iter().all(annotation_is_plain),
        Expr::BinOp(b) => {
            matches!(b.op, Operator::BitOr)
                && annotation_is_plain(&b.left)
                && annotation_is_plain(&b.right)
        }
        Expr::NoneLiteral(_) | Expr::EllipsisLiteral(_) => true,
        _ => false,
    }
}

struct Collector<'a> {
    scopes: Vec<Scope<'a>>,
    current: usize,
    occurrences: Vec<Occurrence>,
    pending_nonlocal: Vec<(usize, String)>,
    // `Cls.NAME` loads, resolved to a class attribute once every scope is known.
    attribute_occurrences: Vec<AttributeOccurrence>,
    // `Cls.NAME = ...` / `del Cls.NAME` / `Cls.NAME += ...`: (scope, `Cls`, `NAME`).
    pending_attribute_stores: Vec<(usize, String, String)>,
    // `setattr(Cls, ...)` / `delattr(Cls, ...)` / `vars(Cls)`: (scope, `Cls`).
    dynamic_class_targets: Vec<(usize, String)>,
    // `globals()`/`vars()`/`exec`/`eval`/`locals()` or a star import: the module
    // namespace can change in ways no binding records.
    dynamic_namespace: bool,
    builtin: Option<&'static str>,
}

struct AttributeOccurrence {
    range: TextRange,
    scope: usize,
    base: String,
    attr: String,
    use_: Use,
}

impl<'a> Collector<'a> {
    fn new() -> Self {
        Collector {
            scopes: vec![Scope {
                kind: ScopeKind::Module,
                parent: None,
                bindings: HashMap::new(),
                globals: HashSet::new(),
                nonlocals: HashSet::new(),
            }],
            current: 0,
            occurrences: Vec::new(),
            pending_nonlocal: Vec::new(),
            attribute_occurrences: Vec::new(),
            pending_attribute_stores: Vec::new(),
            dynamic_class_targets: Vec::new(),
            dynamic_namespace: false,
            builtin: None,
        }
    }

    fn push_scope(&mut self, kind: ScopeKind) -> usize {
        self.scopes.push(Scope {
            kind,
            parent: Some(self.current),
            bindings: HashMap::new(),
            globals: HashSet::new(),
            nonlocals: HashSet::new(),
        });
        let id = self.scopes.len() - 1;
        self.current = id;
        id
    }

    fn bind_in(&mut self, scope: usize, name: &str, binding: Binding<'a>) {
        if self.scopes[scope].globals.contains(name) {
            self.scopes[0]
                .bindings
                .entry(name.to_string())
                .or_default()
                .push(binding);
        } else if self.scopes[scope].nonlocals.contains(name) {
            self.pending_nonlocal.push((scope, name.to_string()));
        } else {
            self.scopes[scope]
                .bindings
                .entry(name.to_string())
                .or_default()
                .push(binding);
        }
    }

    fn bind(&mut self, name: &str, binding: Binding<'a>) {
        self.bind_in(self.current, name, binding);
    }

    // A walrus inside a comprehension binds in the enclosing function/module scope.
    fn bind_walrus(&mut self, name: &str) {
        let mut scope = self.current;
        while self.scopes[scope].kind == ScopeKind::Comprehension {
            scope = self.scopes[scope].parent.unwrap_or(0);
        }
        self.bind_in(scope, name, Binding::Other);
    }

    fn occurrence(&mut self, name: &ast::ExprName, use_: Use) {
        if name.ctx != ExprContext::Load {
            return;
        }
        self.occurrences.push(Occurrence {
            offset: name.range().start(),
            scope: self.current,
            name: name.id.to_string(),
            use_,
            builtin: self.builtin,
        });
    }

    fn body(&mut self, body: &'a [ast::Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    fn parameters(&mut self, parameters: &'a ast::Parameters) {
        for p in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            self.bind(p.parameter.name.as_str(), Binding::Other);
        }
        for p in parameters.vararg.iter().chain(parameters.kwarg.iter()) {
            self.bind(p.name.as_str(), Binding::Other);
        }
    }

    // Defaults are evaluated, once, in the enclosing scope, and kept by the function.
    fn parameter_defaults(&mut self, parameters: &'a ast::Parameters) {
        for p in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            if let Some(default) = &p.default {
                self.expr(default, Use::Escapes);
            }
        }
    }

    // An assignment, `for`, `with` or `del` target.
    fn target(&mut self, target: &'a Expr) {
        match target {
            Expr::Name(n) => self.bind(n.id.as_str(), Binding::Other),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.target(e)),
            Expr::List(l) => l.elts.iter().for_each(|e| self.target(e)),
            Expr::Starred(s) => self.target(&s.value),
            // `x[k] = v`, `x.a = v`, `del x[k]` change the object `x` refers to.
            Expr::Subscript(s) => {
                self.expr(&s.value, Use::Escapes);
                self.expr(&s.slice, Use::Safe);
            }
            Expr::Attribute(a) => {
                self.expr(&a.value, Use::Escapes);
                if let Expr::Name(base) = &*a.value {
                    self.pending_attribute_stores.push((
                        self.current,
                        base.id.to_string(),
                        a.attr.to_string(),
                    ));
                }
            }
            _ => {}
        }
    }

    fn pattern(&mut self, pattern: &'a ast::Pattern) {
        match pattern {
            ast::Pattern::MatchValue(p) => self.expr(&p.value, Use::Safe),
            ast::Pattern::MatchSingleton(_) => {}
            ast::Pattern::MatchSequence(p) => p.patterns.iter().for_each(|q| self.pattern(q)),
            ast::Pattern::MatchMapping(p) => {
                p.keys.iter().for_each(|k| self.expr(k, Use::Safe));
                p.patterns.iter().for_each(|q| self.pattern(q));
                if let Some(rest) = &p.rest {
                    self.bind(rest.as_str(), Binding::Other);
                }
            }
            ast::Pattern::MatchClass(p) => {
                self.expr(&p.cls, Use::Safe);
                p.arguments.patterns.iter().for_each(|q| self.pattern(q));
                p.arguments
                    .keywords
                    .iter()
                    .for_each(|k| self.pattern(&k.pattern));
            }
            ast::Pattern::MatchStar(p) => {
                if let Some(name) = &p.name {
                    self.bind(name.as_str(), Binding::Other);
                }
            }
            ast::Pattern::MatchAs(p) => {
                if let Some(inner) = &p.pattern {
                    self.pattern(inner);
                }
                if let Some(name) = &p.name {
                    self.bind(name.as_str(), Binding::Other);
                }
            }
            ast::Pattern::MatchOr(p) => p.patterns.iter().for_each(|q| self.pattern(q)),
        }
    }

    fn stmt(&mut self, stmt: &'a ast::Stmt) {
        use ast::Stmt;
        match stmt {
            Stmt::FunctionDef(f) => {
                for d in &f.decorator_list {
                    self.expr(&d.expression, Use::Escapes);
                }
                self.parameter_defaults(&f.parameters);
                self.bind(f.name.as_str(), Binding::Function(f));
                let outer = self.current;
                self.push_scope(ScopeKind::Function);
                self.parameters(&f.parameters);
                self.body(&f.body);
                self.current = outer;
            }
            Stmt::ClassDef(c) => {
                for d in &c.decorator_list {
                    self.expr(&d.expression, Use::Escapes);
                }
                if let Some(arguments) = &c.arguments {
                    arguments
                        .args
                        .iter()
                        .for_each(|a| self.expr(a, Use::Escapes));
                    arguments
                        .keywords
                        .iter()
                        .for_each(|k| self.expr(&k.value, Use::Escapes));
                }
                let outer = self.current;
                let body_scope = self.push_scope(ScopeKind::Class);
                self.bind_in(outer, c.name.as_str(), Binding::Class(c, body_scope));
                self.body(&c.body);
                self.current = outer;
            }
            Stmt::Return(r) => {
                if let Some(v) = &r.value {
                    self.expr(v, Use::Escapes);
                }
            }
            Stmt::Delete(d) => d.targets.iter().for_each(|t| self.target(t)),
            Stmt::Assign(a) => {
                // `a, b = x` reads x's elements; any other target keeps a reference to x.
                let unpacking = a
                    .targets
                    .iter()
                    .all(|t| matches!(t, Expr::Tuple(_) | Expr::List(_)));
                self.expr(
                    &a.value,
                    if unpacking {
                        Use::Interior
                    } else {
                        Use::Escapes
                    },
                );
                match a.targets.as_slice() {
                    [Expr::Name(n)] => self.bind(n.id.as_str(), Binding::Assign(&a.value)),
                    targets => targets.iter().for_each(|t| self.target(t)),
                }
            }
            Stmt::AugAssign(a) => {
                self.expr(&a.value, Use::Escapes);
                match &*a.target {
                    Expr::Name(n) => {
                        // `x += y` reads and rebinds `x`.
                        self.occurrence_force(n, Use::Escapes);
                        self.bind(n.id.as_str(), Binding::Other);
                    }
                    other => self.target(other),
                }
            }
            Stmt::AnnAssign(a) => {
                if let Some(v) = &a.value {
                    self.expr(v, Use::Escapes);
                }
                match (&*a.target, &a.value) {
                    // An annotation can declare the name to be something its value isn't
                    // (`df: Annotated[pl.DataFrame, S] = None` is a DataFrame to the
                    // checker), so only a plain builtin type leaves the value as the truth.
                    (Expr::Name(n), Some(v)) if annotation_is_plain(&a.annotation) => {
                        self.bind(n.id.as_str(), Binding::Assign(v))
                    }
                    (Expr::Name(n), Some(_)) => self.bind(n.id.as_str(), Binding::Other),
                    (Expr::Name(n), None) => self.bind(n.id.as_str(), Binding::Other),
                    (other, _) => self.target(other),
                }
            }
            Stmt::For(f) => {
                self.expr(&f.iter, Use::Interior);
                self.target(&f.target);
                self.body(&f.body);
                self.body(&f.orelse);
            }
            Stmt::While(w) => {
                self.expr(&w.test, Use::Safe);
                self.body(&w.body);
                self.body(&w.orelse);
            }
            Stmt::If(i) => {
                self.expr(&i.test, Use::Safe);
                self.body(&i.body);
                for clause in &i.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.expr(test, Use::Safe);
                    }
                    self.body(&clause.body);
                }
            }
            Stmt::With(w) => {
                for item in &w.items {
                    self.expr(&item.context_expr, Use::Escapes);
                    if let Some(vars) = &item.optional_vars {
                        self.target(vars);
                    }
                }
                self.body(&w.body);
            }
            Stmt::Match(m) => {
                self.expr(&m.subject, Use::Escapes);
                for case in &m.cases {
                    self.pattern(&case.pattern);
                    if let Some(guard) = &case.guard {
                        self.expr(guard, Use::Safe);
                    }
                    self.body(&case.body);
                }
            }
            Stmt::Raise(r) => {
                r.exc
                    .iter()
                    .chain(r.cause.iter())
                    .for_each(|e| self.expr(e, Use::Escapes));
            }
            Stmt::Try(t) => {
                self.body(&t.body);
                for handler in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    if let Some(type_) = &h.type_ {
                        self.expr(type_, Use::Safe);
                    }
                    if let Some(name) = &h.name {
                        self.bind(name.as_str(), Binding::Other);
                    }
                    self.body(&h.body);
                }
                self.body(&t.orelse);
                self.body(&t.finalbody);
            }
            Stmt::Assert(a) => {
                self.expr(&a.test, Use::Safe);
                if let Some(msg) = &a.msg {
                    self.expr(msg, Use::Safe);
                }
            }
            Stmt::Import(i) => {
                for alias in &i.names {
                    let (bound, binding) = match &alias.asname {
                        Some(asname) => (
                            asname.as_str(),
                            Binding::ImportModule(alias.name.to_string()),
                        ),
                        // `import a.b` binds `a`, the package, not the module.
                        None if alias.name.as_str().contains('.') => (
                            alias.name.as_str().split('.').next().unwrap_or(""),
                            Binding::Other,
                        ),
                        None => (
                            alias.name.as_str(),
                            Binding::ImportModule(alias.name.to_string()),
                        ),
                    };
                    self.bind(bound, binding);
                }
            }
            Stmt::ImportFrom(i) => {
                for alias in &i.names {
                    if alias.name.as_str() == "*" {
                        self.dynamic_namespace = true;
                    } else {
                        let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                        let module = i.module.as_ref().map(|m| m.to_string());
                        self.bind(
                            bound.as_str(),
                            Binding::ImportFrom(
                                i.level,
                                module.unwrap_or_default(),
                                alias.name.to_string(),
                            ),
                        );
                    }
                }
            }
            Stmt::Global(g) => {
                for name in &g.names {
                    self.scopes[self.current].globals.insert(name.to_string());
                }
            }
            Stmt::Nonlocal(n) => {
                for name in &n.names {
                    self.scopes[self.current].nonlocals.insert(name.to_string());
                }
            }
            Stmt::TypeAlias(t) => self.target(&t.name),
            Stmt::Expr(e) => self.expr(&e.value, Use::Safe),
            Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    // A name that is read and rebound in one statement (`x += y`): ruff gives its `ctx`
    // as Store, but it is a use all the same.
    fn occurrence_force(&mut self, name: &ast::ExprName, use_: Use) {
        self.occurrences.push(Occurrence {
            offset: name.range().start(),
            scope: self.current,
            name: name.id.to_string(),
            use_,
            builtin: None,
        });
    }

    fn expr(&mut self, expr: &'a Expr, use_: Use) {
        match expr {
            Expr::Name(n) => self.occurrence(n, use_),
            Expr::Attribute(a) => {
                if let Expr::Name(base) = &*a.value {
                    if a.ctx == ExprContext::Load {
                        self.attribute_occurrences.push(AttributeOccurrence {
                            range: a.range(),
                            scope: self.current,
                            base: base.id.to_string(),
                            attr: a.attr.to_string(),
                            use_,
                        });
                    }
                }
                self.expr(&a.value, Use::Escapes);
            }
            // The result of a subscript is an element (or a copy of part) of its base, so the
            // base is exposed at most to the interior: a mutable element lets it be changed
            // unseen, an immutable one can't. Consumed `Safe` stays `Safe`.
            Expr::Subscript(s) => {
                self.expr(&s.value, use_.min(Use::Interior));
                self.expr(&s.slice, Use::Safe);
            }
            Expr::Slice(s) => {
                s.lower
                    .iter()
                    .chain(s.upper.iter())
                    .chain(s.step.iter())
                    .for_each(|e| self.expr(e, Use::Safe));
            }
            Expr::Call(c) => self.call(c),
            Expr::BinOp(b) => {
                self.expr(&b.left, Use::Interior);
                self.expr(&b.right, Use::Interior);
            }
            Expr::UnaryOp(u) => self.expr(&u.operand, Use::Safe),
            // `a or b` evaluates to one of its operands.
            Expr::BoolOp(b) => b.values.iter().for_each(|v| self.expr(v, use_)),
            Expr::Compare(c) => {
                self.expr(&c.left, Use::Safe);
                c.comparators.iter().for_each(|e| self.expr(e, Use::Safe));
            }
            Expr::If(i) => {
                self.expr(&i.test, Use::Safe);
                self.expr(&i.body, use_);
                self.expr(&i.orelse, use_);
            }
            Expr::List(l) => self.elements(&l.elts),
            Expr::Tuple(t) => self.elements(&t.elts),
            Expr::Set(s) => self.elements(&s.elts),
            Expr::Dict(d) => {
                for item in &d.items {
                    match &item.key {
                        Some(key) => {
                            self.expr(key, Use::Safe);
                            self.expr(&item.value, Use::Escapes);
                        }
                        None => self.expr(&item.value, Use::Interior),
                    }
                }
            }
            Expr::Starred(s) => self.expr(&s.value, Use::Escapes),
            Expr::FString(f) => self.interpolations(
                f.value
                    .as_slice()
                    .iter()
                    .filter_map(|p| match p {
                        ast::FStringPart::FString(f) => Some(f.elements.iter()),
                        ast::FStringPart::Literal(_) => None,
                    })
                    .flatten(),
            ),
            Expr::TString(t) => self.interpolations(t.value.elements()),
            Expr::Lambda(l) => {
                if let Some(parameters) = &l.parameters {
                    self.parameter_defaults(parameters);
                }
                let outer = self.current;
                self.push_scope(ScopeKind::Function);
                if let Some(parameters) = &l.parameters {
                    self.parameters(parameters);
                }
                self.expr(&l.body, Use::Escapes);
                self.current = outer;
            }
            Expr::ListComp(c) => self.comprehension(&c.generators, &[(&c.elt, Use::Escapes)]),
            Expr::SetComp(c) => self.comprehension(&c.generators, &[(&c.elt, Use::Escapes)]),
            Expr::Generator(c) => self.comprehension(&c.generators, &[(&c.elt, Use::Escapes)]),
            Expr::DictComp(c) => {
                let mut parts: Vec<(&'a Expr, Use)> = Vec::new();
                if let Some(key) = &c.key {
                    parts.push((key, Use::Safe));
                }
                parts.push((&c.value, Use::Escapes));
                self.comprehension(&c.generators, &parts);
            }
            Expr::Await(a) => self.expr(&a.value, Use::Escapes),
            Expr::Yield(y) => y.value.iter().for_each(|v| self.expr(v, Use::Escapes)),
            Expr::YieldFrom(y) => self.expr(&y.value, Use::Escapes),
            Expr::Named(n) => {
                self.expr(&n.value, Use::Escapes);
                if let Expr::Name(target) = &*n.target {
                    self.bind_walrus(target.id.as_str());
                }
            }
            Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::IpyEscapeCommand(_) => {}
        }
    }

    // List/tuple/set display: an element is stored in the new container; `*x` only
    // reads x's elements.
    fn elements(&mut self, elts: &'a [Expr]) {
        for elt in elts {
            match elt {
                Expr::Starred(s) => self.expr(&s.value, Use::Interior),
                other => self.expr(other, Use::Escapes),
            }
        }
    }

    fn interpolations<'e>(
        &mut self,
        elements: impl Iterator<Item = &'a ast::InterpolatedStringElement> + 'e,
    ) where
        'a: 'e,
    {
        for element in elements {
            if let ast::InterpolatedStringElement::Interpolation(interp) = element {
                self.expr(&interp.expression, Use::Safe);
                if let Some(spec) = &interp.format_spec {
                    self.interpolations(spec.elements.iter());
                }
            }
        }
    }

    // The first iterable is evaluated in the enclosing scope; everything else in the
    // comprehension's own.
    fn comprehension(
        &mut self,
        generators: &'a [ast::Comprehension],
        elements: &[(&'a Expr, Use)],
    ) {
        if let Some(first) = generators.first() {
            self.expr(&first.iter, Use::Interior);
        }
        let outer = self.current;
        self.push_scope(ScopeKind::Comprehension);
        for (i, generator) in generators.iter().enumerate() {
            self.target(&generator.target);
            if i > 0 {
                self.expr(&generator.iter, Use::Interior);
            }
            for condition in &generator.ifs {
                self.expr(condition, Use::Safe);
            }
        }
        for (element, use_) in elements {
            self.expr(element, *use_);
        }
        self.current = outer;
    }

    fn call(&mut self, call: &'a ast::ExprCall) {
        let mut arg_use = Use::Escapes;
        let mut builtin: Option<&'static str> = None;
        match &*call.func {
            Expr::Name(f) => {
                self.occurrence(f, Use::Safe);
                let name = f.id.as_str();
                if matches!(name, "globals" | "vars" | "exec" | "eval" | "locals") {
                    self.dynamic_namespace = true;
                }
                // These can set attributes on a class without naming them.
                if matches!(name, "setattr" | "delattr" | "vars") {
                    if let Some(Expr::Name(target)) = call.arguments.args.first() {
                        self.dynamic_class_targets
                            .push((self.current, target.id.to_string()));
                    }
                }
                if let Some(b) = SAFE_BUILTINS.iter().find(|b| **b == name) {
                    (arg_use, builtin) = (Use::Safe, Some(*b));
                } else if let Some(b) = INTERIOR_BUILTINS.iter().find(|b| **b == name) {
                    (arg_use, builtin) = (Use::Interior, Some(*b));
                } else if let Some(b) = LOAD_FUNCTIONS.iter().find(|b| **b == name) {
                    // Trusted not to change what it is given, unless the file defines its own.
                    (arg_use, builtin) = (Use::Safe, Some(*b));
                }
            }
            Expr::Attribute(a) => {
                let method = a.attr.as_str();
                // `pd.concat(...)`, `pl.col(...)`: a call through the pandas/polars module.
                let module_call =
                    matches!(&*a.value, Expr::Name(n) if LOAD_MODULES.contains(&n.id.as_str()));
                let (receiver, args) = if LOAD_FUNCTIONS.contains(&method)
                    || SAFE_METHODS.contains(&method)
                    || module_call
                {
                    (Use::Safe, Use::Safe)
                } else if method == "join" {
                    (Use::Safe, Use::Interior)
                } else if INTERIOR_METHODS.contains(&method) {
                    (Use::Interior, Use::Safe)
                } else if FRAME_METHODS.contains(&method) {
                    // The receiver is a frame (or a static value it makes no sense to call
                    // this on): either way it is not the arguments' concern.
                    (Use::Escapes, Use::Safe)
                } else {
                    (Use::Escapes, Use::Escapes)
                };
                // `cfg.cols(...)` may be a call to a function of an imported module.
                if let Expr::Name(base) = &*a.value {
                    self.attribute_occurrences.push(AttributeOccurrence {
                        range: a.range(),
                        scope: self.current,
                        base: base.id.to_string(),
                        attr: a.attr.to_string(),
                        use_: Use::Safe,
                    });
                }
                self.expr(&a.value, receiver);
                arg_use = args;
            }
            other => self.expr(other, Use::Escapes),
        }
        let outer = self.builtin.replace_with(builtin);
        for arg in call.arguments.args.iter() {
            match arg {
                Expr::Starred(s) => self.expr(&s.value, Use::Escapes),
                other => self.expr(other, arg_use),
            }
        }
        for keyword in call.arguments.keywords.iter() {
            let use_ = if keyword.arg.is_some() {
                arg_use
            } else {
                Use::Escapes
            };
            self.expr(&keyword.value, use_);
        }
        self.builtin = outer;
    }
}

trait ReplaceWith<T> {
    fn replace_with(&mut self, value: T) -> T;
}

impl<T> ReplaceWith<Option<T>> for Option<T> {
    fn replace_with(&mut self, value: Option<T>) -> Option<T> {
        std::mem::replace(self, value)
    }
}

// Every load of a name in the module, other than in annotations and type parameters
// (which the collector does not classify). Used to check the collector missed none: a
// missed use could hide a mutation.
struct AllNameLoads {
    offsets: HashSet<TextSize>,
}

impl<'a> Visitor<'a> for AllNameLoads {
    fn visit_annotation(&mut self, _expr: &'a Expr) {}
    fn visit_type_params(&mut self, _type_params: &'a ast::TypeParams) {}
    // `type X = ...` is a type-level alias, like an annotation.
    fn visit_stmt(&mut self, stmt: &'a ast::Stmt) {
        if !matches!(stmt, ast::Stmt::TypeAlias(_)) {
            ast_visitor::walk_stmt(self, stmt);
        }
    }
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(n) = expr {
            if n.ctx == ExprContext::Load {
                self.offsets.insert(n.range().start());
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

#[derive(Clone)]
enum ImportRef {
    From(u32, String, String),
    Module(String),
}

#[derive(Default)]
struct Group<'a> {
    bindings: usize,
    assign: Option<&'a Expr>,
    function: Option<&'a ast::StmtFunctionDef>,
    class: Option<(&'a ast::StmtClassDef, usize)>,
    import: Option<ImportRef>,
    interior: bool,
    escapes: bool,
}

enum Memo {
    InProgress,
    Done(Option<Value>),
}

pub(crate) struct Analysis<'a> {
    groups: HashMap<GroupKey, Group<'a>>,
    occurrence_group: HashMap<TextSize, Option<GroupKey>>,
    attribute_group: HashMap<TextRange, Option<GroupKey>>,
    // `cfg.NAME` where `cfg` is an imported module: the module's group and the member,
    // plus how this file uses that member (every use of it, across the whole file).
    module_attribute: HashMap<TextRange, (GroupKey, String)>,
    module_member_use: HashMap<(GroupKey, String), (bool, bool)>,
    imports: &'a ImportedModules,
    shadowed: Vec<String>,
    complete: bool,
    dynamic_namespace: bool,
    memo: RefCell<HashMap<GroupKey, Memo>>,
    depth: Cell<u32>,
    // Functions being evaluated right now (a recursive one is refused) and how many calls
    // have been evaluated in total.
    call_stack: RefCell<Vec<GroupKey>>,
    calls: Cell<u32>,
}

const MAX_CALL_DEPTH: usize = 16;
const MAX_CALLS: u32 = 10_000;

impl<'a> Analysis<'a> {
    pub(crate) fn build(module: &'a ModModule, imports: &'a ImportedModules) -> Self {
        let mut collector = Collector::new();
        collector.body(&module.body);

        let mut loads = AllNameLoads {
            offsets: HashSet::new(),
        };
        for stmt in &module.body {
            loads.visit_stmt(stmt);
        }
        let recorded: HashSet<TextSize> = collector.occurrences.iter().map(|o| o.offset).collect();
        let complete = loads.offsets.iter().all(|o| recorded.contains(o));

        let scopes = &collector.scopes;
        let mut groups: HashMap<GroupKey, Group<'a>> = HashMap::new();
        for (id, scope) in scopes.iter().enumerate() {
            for (name, bindings) in &scope.bindings {
                let group = groups.entry((id, name.clone())).or_default();
                group.bindings += bindings.len();
                if let [only] = bindings.as_slice() {
                    match only {
                        Binding::Assign(rhs) => group.assign = Some(rhs),
                        Binding::Function(f) => group.function = Some(f),
                        Binding::Class(c, body) => group.class = Some((c, *body)),
                        Binding::ImportFrom(level, module, name) => {
                            group.import =
                                Some(ImportRef::From(*level, module.clone(), name.clone()))
                        }
                        Binding::ImportModule(module) => {
                            group.import = Some(ImportRef::Module(module.clone()))
                        }
                        Binding::Other => {}
                    }
                }
            }
        }
        for (scope, name) in &collector.pending_nonlocal {
            if let Some(key) = lookup(scopes, *scope, name) {
                groups.entry(key).or_default().bindings += 1;
            }
        }

        // A class whose attributes `setattr`/`delattr`/`vars` may change, and stores to a
        // class attribute, which are one more binding of it.
        let mut dynamic_classes: HashSet<usize> = HashSet::new();
        for (scope, name) in &collector.dynamic_class_targets {
            if let Some((_, body)) = lookup(scopes, *scope, name)
                .and_then(|key| groups.get(&key))
                .and_then(|g| g.class)
            {
                dynamic_classes.insert(body);
            }
        }
        for (scope, base, attr) in &collector.pending_attribute_stores {
            if let Some(key) =
                class_attribute(&groups, scopes, &dynamic_classes, *scope, base, attr)
            {
                if let Some(group) = groups.get_mut(&key) {
                    group.bindings += 1;
                }
            }
        }

        let bound_anywhere = |name: &str| scopes.iter().any(|s| s.bindings.contains_key(name));
        let mut occurrence_group = HashMap::new();
        for occurrence in &collector.occurrences {
            let key = lookup(scopes, occurrence.scope, &occurrence.name)
                .filter(|key| groups.contains_key(key));
            let mut use_ = occurrence.use_;
            if occurrence.builtin.is_some_and(bound_anywhere) {
                use_ = Use::Escapes;
            }
            if let Some(key) = &key {
                if let Some(group) = groups.get_mut(key) {
                    match use_ {
                        Use::Safe => {}
                        Use::Interior => group.interior = true,
                        Use::Escapes => group.escapes = true,
                    }
                }
            }
            occurrence_group.insert(occurrence.offset, key);
        }

        let mut attribute_group = HashMap::new();
        let mut module_attribute = HashMap::new();
        let mut module_member_use: HashMap<(GroupKey, String), (bool, bool)> = HashMap::new();
        for occurrence in &collector.attribute_occurrences {
            let key = class_attribute(
                &groups,
                scopes,
                &dynamic_classes,
                occurrence.scope,
                &occurrence.base,
                &occurrence.attr,
            );
            if let Some(group) = key.as_ref().and_then(|k| groups.get_mut(k)) {
                match occurrence.use_ {
                    Use::Safe => {}
                    Use::Interior => group.interior = true,
                    Use::Escapes => group.escapes = true,
                }
            }
            if key.is_none() {
                let module_group = lookup(scopes, occurrence.scope, &occurrence.base)
                    .filter(|k| groups.get(k).is_some_and(|g| g.bindings == 1));
                if let Some(base) = module_group.filter(|k| {
                    matches!(
                        groups.get(k).and_then(|g| g.import.as_ref()),
                        Some(ImportRef::Module(_))
                    )
                }) {
                    let member = (base.clone(), occurrence.attr.clone());
                    let flags = module_member_use.entry(member.clone()).or_default();
                    match occurrence.use_ {
                        Use::Safe => {}
                        Use::Interior => flags.0 = true,
                        Use::Escapes => flags.1 = true,
                    }
                    module_attribute.insert(occurrence.range, member);
                }
            }
            attribute_group.insert(occurrence.range, key);
        }

        Analysis {
            groups,
            occurrence_group,
            attribute_group,
            module_attribute,
            module_member_use,
            imports,
            shadowed: EVALUATED_BUILTINS
                .iter()
                .filter(|b| bound_anywhere(b))
                .map(|b| b.to_string())
                .collect(),
            complete,
            dynamic_namespace: collector.dynamic_namespace,
            memo: RefCell::new(HashMap::new()),
            depth: Cell::new(0),
            call_stack: RefCell::new(Vec::new()),
            calls: Cell::new(0),
        }
    }

    fn group_value(&self, key: &GroupKey) -> Option<Value> {
        match self.memo.borrow().get(key) {
            Some(Memo::InProgress) => return None,
            Some(Memo::Done(v)) => return v.clone(),
            None => {}
        }
        let depth = self.depth.get() + 1;
        if depth > MAX_DEPTH {
            return None;
        }
        self.depth.set(depth);
        self.memo.borrow_mut().insert(key.clone(), Memo::InProgress);
        let value = self.compute(key);
        self.depth.set(depth - 1);
        self.memo
            .borrow_mut()
            .insert(key.clone(), Memo::Done(value.clone()));
        value
    }

    fn compute(&self, key: &GroupKey) -> Option<Value> {
        let group = self.groups.get(key)?;
        // One plain assignment, and a module-level name only if nothing can rewrite the
        // module namespace behind the bindings' backs.
        if group.bindings != 1 || (key.0 == 0 && self.dynamic_namespace) {
            return None;
        }
        let value = match (&group.assign, &group.import) {
            (Some(rhs), _) => Evaluator::new(self).eval(rhs)?,
            (None, Some(ImportRef::From(level, module, name))) => self
                .imports
                .get(&(*level, module.clone()))?
                .constants
                .get(name)?
                .clone(),
            _ => return None,
        };
        // A value nothing can change may be used any way at all. A container may not be
        // aliased, passed on or stored (any of which could change it unseen), and may
        // hand out its elements only if they are immutable.
        if group.escapes && !value.is_immutable() {
            return None;
        }
        if group.interior && !value.has_immutable_elements() {
            return None;
        }
        Some(value)
    }
}

impl NameResolver for Analysis<'_> {
    fn resolve(&self, name: &ast::ExprName) -> Resolved {
        match self.occurrence_group.get(&name.range().start()) {
            Some(Some(key)) => self
                .group_value(key)
                .map_or(Resolved::Unknown, Resolved::Value),
            Some(None) => Resolved::Builtin,
            None => Resolved::Unknown,
        }
    }

    fn resolve_attribute(&self, attribute: &ast::ExprAttribute) -> Resolved {
        match self.attribute_group.get(&attribute.range()) {
            Some(Some(key)) => {
                return self
                    .group_value(key)
                    .map_or(Resolved::Unknown, Resolved::Value)
            }
            Some(None) => {}
            None => return Resolved::Unknown,
        }
        let Some(member) = self.imported_member(attribute) else {
            return Resolved::Unknown;
        };
        let Some(value) = member.constants.get(attribute.attr.as_str()) else {
            return Resolved::Unknown;
        };
        // Same rule as for any constant: a container this file aliases, passes on or
        // hands out the elements of must not be treated as fixed.
        let (interior, escapes) = self
            .module_attribute
            .get(&attribute.range())
            .and_then(|m| self.module_member_use.get(m))
            .copied()
            .unwrap_or((true, true));
        if (escapes && !value.is_immutable()) || (interior && !value.has_immutable_elements()) {
            return Resolved::Unknown;
        }
        Resolved::Value(value.clone())
    }

    fn is_module_member(&self, attribute: &ast::ExprAttribute) -> bool {
        self.imported_member(attribute)
            .is_some_and(|m| m.functions.contains_key(attribute.attr.as_str()))
    }

    fn call_module_function(
        &self,
        attribute: &ast::ExprAttribute,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        let module = self.imported_member(attribute)?;
        self.call_foreign(module, attribute.attr.as_str(), args, kwargs)
    }

    fn call_function(
        &self,
        callee: &ast::ExprName,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        let key = self
            .occurrence_group
            .get(&callee.range().start())?
            .clone()?;
        let group = self.groups.get(&key)?;
        if let Some(ImportRef::From(level, module, name)) = &group.import {
            if group.bindings != 1 || (key.0 == 0 && self.dynamic_namespace) {
                return None;
            }
            let exports = self.imports.get(&(*level, module.clone()))?;
            return self.call_foreign(exports, name, args, kwargs);
        }
        let def = group.function?;
        if group.bindings != 1
            || (key.0 == 0 && self.dynamic_namespace)
            || def.is_async
            || !def.decorator_list.is_empty()
            || def.parameters.vararg.is_some()
            || def.parameters.kwarg.is_some()
        {
            return None;
        }
        // No recursion, and a ceiling on the work one file can ask for.
        {
            let stack = self.call_stack.borrow();
            if stack.contains(&key) || stack.len() >= MAX_CALL_DEPTH {
                return None;
            }
        }
        let calls = self.calls.get() + 1;
        if calls > MAX_CALLS {
            return None;
        }
        self.calls.set(calls);

        let bound = bind_arguments(&def.parameters, args, kwargs, |default| {
            Evaluator::new(self).eval(default)
        })?;

        self.call_stack.borrow_mut().push(key);
        let result = Evaluator::new(self).run_body(bound, &def.body);
        self.call_stack.borrow_mut().pop();
        result
    }
}

/// What a bare name in call position refers to, when exactly one thing can.
pub(crate) enum CalleeBinding<'a> {
    /// A function defined in this file.
    Function(&'a ast::StmtFunctionDef),
    /// A name brought in by `from module import name [as local]`.
    Imported { local: String },
}

impl<'a> Analysis<'a> {
    /// Whether this occurrence of a name refers to nothing the file binds anywhere, so it can
    /// only be a builtin.
    pub(crate) fn is_unbound_name(&self, name: &ast::ExprName) -> bool {
        matches!(self.resolve(name), Resolved::Builtin)
    }

    /// The one plain, undecorated function a module-level `name` is bound to.
    pub(crate) fn single_module_function(&self, name: &str) -> Option<&'a ast::StmtFunctionDef> {
        if self.dynamic_namespace {
            return None;
        }
        let group = self.groups.get(&(0, name.to_string()))?;
        let def = group.function?;
        (group.bindings == 1 && is_plain_function(def) && def.decorator_list.is_empty())
            .then_some(def)
    }

    /// The single function or import the name `callee` refers to at this occurrence: bound
    /// once in its scope, in a namespace nothing can rewrite behind the bindings' backs, and
    /// (for a function) undecorated and not `async`.
    pub(crate) fn callee_binding(&self, callee: &ast::ExprName) -> Option<CalleeBinding<'a>> {
        let key = self
            .occurrence_group
            .get(&callee.range().start())?
            .as_ref()?;
        let group = self.groups.get(key)?;
        if group.bindings != 1 || (key.0 == 0 && self.dynamic_namespace) {
            return None;
        }
        if let Some(def) = group.function {
            return (is_plain_function(def) && def.decorator_list.is_empty())
                .then_some(CalleeBinding::Function(def));
        }
        matches!(group.import, Some(ImportRef::From(..))).then(|| CalleeBinding::Imported {
            local: key.1.clone(),
        })
    }
}

impl Analysis<'_> {
    // The exports of the project module a `cfg.NAME` reaches through `import cfg`.
    fn imported_member(&self, attribute: &ast::ExprAttribute) -> Option<&Arc<ModuleExports>> {
        let (key, _) = self.module_attribute.get(&attribute.range())?;
        if key.0 == 0 && self.dynamic_namespace {
            return None;
        }
        let ImportRef::Module(module) = self.groups.get(key)?.import.as_ref()? else {
            return None;
        };
        self.imports.get(&(0, module.clone()))
    }

    fn call_foreign(
        &self,
        exports: &ModuleExports,
        name: &str,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        let calls = self.calls.get() + 1;
        if calls > MAX_CALLS {
            return None;
        }
        self.calls.set(calls);
        let run = ForeignRun {
            exports,
            stack: RefCell::new(Vec::new()),
            calls: Cell::new(0),
        };
        run.call(name, args, kwargs)
    }
}

// Binds a call's arguments to a function's parameters. A default is evaluated by
// `default`, in the scope the function was defined in, not where it is called.
fn bind_arguments(
    parameters: &ast::Parameters,
    args: Vec<Value>,
    kwargs: Vec<(String, Value)>,
    default: impl Fn(&Expr) -> Option<Value>,
) -> Option<HashMap<String, Value>> {
    let positional: Vec<&ast::ParameterWithDefault> = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .collect();
    if args.len() > positional.len() {
        return None;
    }
    let mut bound: HashMap<String, Value> = HashMap::new();
    for (param, value) in positional.iter().zip(args) {
        bound.insert(param.parameter.name.to_string(), value);
    }
    for (name, value) in kwargs {
        let known = parameters
            .args
            .iter()
            .chain(parameters.kwonlyargs.iter())
            .any(|p| p.parameter.name.as_str() == name);
        if !known || bound.contains_key(&name) {
            return None;
        }
        bound.insert(name, value);
    }
    for param in positional
        .iter()
        .copied()
        .chain(parameters.kwonlyargs.iter())
    {
        let name = param.parameter.name.as_str();
        if !bound.contains_key(name) {
            bound.insert(name.to_string(), default(param.default.as_deref()?)?);
        }
    }
    Some(bound)
}

// ───────────────────────────── cross-file exports ───────────────────────────

/// What one project module offers to the files that import it: its statically known
/// constants and the source of its evaluable functions. Computed when the project is
/// indexed, so an importing file never has to analyse the module again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModuleExports {
    pub(crate) constants: HashMap<String, Value>,
    pub(crate) functions: HashMap<String, String>,
    // Evaluable builtins the module rebinds, so one of its functions isn't evaluated as
    // if `len` were the builtin.
    shadowed: Vec<String>,
}

impl ModuleExports {
    pub(crate) fn is_empty(&self) -> bool {
        self.constants.is_empty() && self.functions.is_empty()
    }
}

/// Imported modules by `(leading dots, dotted name)` as written in the importing file.
pub(crate) type ImportedModules = HashMap<(u32, String), Arc<ModuleExports>>;

const MAX_EXPORTED_NODES: usize = 1_000;

// The builtins the evaluator implements; rebinding one anywhere changes what a call to
// it means.
const EVALUATED_BUILTINS: &[&str] = &[
    "len",
    "list",
    "tuple",
    "set",
    "frozenset",
    "sorted",
    "reversed",
    "range",
    "zip",
    "enumerate",
    "min",
    "max",
    "sum",
    "dict",
    "str",
    "int",
    "bool",
    "any",
    "all",
];

/// The statically known constants and evaluable functions of `module`, for importers.
pub(crate) fn export_module(module: &ModModule, source: &str) -> ModuleExports {
    let none = ImportedModules::new();
    let analysis = Analysis::build(module, &none);
    let mut exports = ModuleExports::default();
    if !analysis.complete || analysis.dynamic_namespace {
        return exports;
    }
    exports.shadowed = analysis.shadowed.clone();
    let mut candidates: HashMap<&str, &ast::StmtFunctionDef> = HashMap::new();
    for ((scope, name), group) in &analysis.groups {
        if *scope != 0 {
            continue;
        }
        if group.assign.is_some() {
            if let Some(value) = analysis.group_value(&(0, name.clone())) {
                if node_count(&value) <= MAX_EXPORTED_NODES {
                    exports.constants.insert(name.clone(), value);
                }
            }
        } else if let Some(def) = group.function {
            if group.bindings == 1 && is_plain_function(def) {
                candidates.insert(name.as_str(), def);
            }
        }
    }
    for (name, def) in &candidates {
        // A local that shares a name with anything the body could otherwise read from the
        // module (a constant, another function, a builtin) might be read before it is
        // assigned; the importing side resolves names without scopes, so refuse it.
        let clashes = assigned_names(&def.body).iter().any(|local| {
            exports.constants.contains_key(local)
                || candidates.contains_key(local.as_str())
                || EVALUATED_BUILTINS.contains(&local.as_str())
        });
        if clashes {
            continue;
        }
        let range = def.range();
        if let Some(text) = source.get(usize::from(range.start())..usize::from(range.end())) {
            exports.functions.insert(name.to_string(), text.to_string());
        }
    }
    exports
}

fn is_plain_function(def: &ast::StmtFunctionDef) -> bool {
    !def.is_async
        && def.decorator_list.is_empty()
        && def.parameters.vararg.is_none()
        && def.parameters.kwarg.is_none()
}

fn assigned_names(body: &[ast::Stmt]) -> Vec<String> {
    use ast::Stmt;
    let mut names = Vec::new();
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                for target in &a.targets {
                    if let Expr::Name(n) = target {
                        names.push(n.id.to_string());
                    }
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = &*a.target {
                    names.push(n.id.to_string());
                }
            }
            _ => {}
        }
    }
    names
}

// One evaluation of a function that lives in another module. Names resolve against that
// module's exports only; the importing file's own names are never visible to it.
struct ForeignRun<'e> {
    exports: &'e ModuleExports,
    stack: RefCell<Vec<String>>,
    calls: Cell<u32>,
}

impl ForeignRun<'_> {
    fn call(&self, name: &str, args: Vec<Value>, kwargs: Vec<(String, Value)>) -> Option<Value> {
        {
            let stack = self.stack.borrow();
            if stack.iter().any(|n| n == name) || stack.len() >= MAX_CALL_DEPTH {
                return None;
            }
        }
        let calls = self.calls.get() + 1;
        if calls > MAX_CALLS {
            return None;
        }
        self.calls.set(calls);

        let text = self.exports.functions.get(name)?;
        let parsed = ruff_python_parser::parse_module(text).ok()?.into_syntax();
        let [ast::Stmt::FunctionDef(def)] = parsed.body.as_slice() else {
            return None;
        };
        let bound = bind_arguments(&def.parameters, args, kwargs, |default| {
            Evaluator::new(self).eval(default)
        })?;
        self.stack.borrow_mut().push(name.to_string());
        let result = Evaluator::new(self).run_body(bound, &def.body);
        self.stack.borrow_mut().pop();
        result
    }
}

impl NameResolver for ForeignRun<'_> {
    fn resolve(&self, name: &ast::ExprName) -> Resolved {
        let id = name.id.as_str();
        if let Some(value) = self.exports.constants.get(id) {
            Resolved::Value(value.clone())
        } else if self.exports.functions.contains_key(id)
            || self.exports.shadowed.iter().any(|s| s == id)
        {
            Resolved::Unknown
        } else {
            Resolved::Builtin
        }
    }

    fn call_function(
        &self,
        callee: &ast::ExprName,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> Option<Value> {
        self.call(callee.id.as_str(), args, kwargs)
    }
}

// The group holding `base.attr` when `base` is a class defined in the file and `attr` is
// assigned in its body (or in its single user-defined base's). Refused for a class a
// decorator, a metaclass, an outside base class (an enum, a `BaseSchema`, an ORM base),
// several bases, a rebinding of its name, or `setattr`/`delattr`/`vars` could have changed.
fn class_attribute(
    groups: &HashMap<GroupKey, Group>,
    scopes: &[Scope],
    dynamic: &HashSet<usize>,
    from: usize,
    base: &str,
    attr: &str,
) -> Option<GroupKey> {
    let mut key = lookup(scopes, from, base).filter(|k| groups.contains_key(k))?;
    let mut chain: Vec<usize> = Vec::new();
    loop {
        let group = groups.get(&key)?;
        let (def, body) = group.class?;
        if group.bindings != 1
            || dynamic.contains(&body)
            || !def.decorator_list.is_empty()
            || chain.len() >= 8
            || chain.contains(&body)
        {
            return None;
        }
        chain.push(body);
        let Some(arguments) = def.arguments.as_deref() else {
            break;
        };
        if !arguments.keywords.is_empty() || arguments.args.len() > 1 {
            return None;
        }
        match arguments.args.first() {
            None => break,
            Some(Expr::Name(parent)) => {
                match lookup(scopes, scopes[body].parent?, parent.id.as_str()) {
                    Some(next) => key = next,
                    None if parent.id.as_str() == "object" => break,
                    None => return None,
                }
            }
            Some(_) => return None,
        }
    }
    chain
        .iter()
        .find(|body| groups.contains_key(&(**body, attr.to_string())))
        .map(|body| (*body, attr.to_string()))
}

// Python's name lookup from `from` outward: the scope's own binding, else the nearest
// enclosing one, not counting class scopes (a method can't see its class's names).
fn lookup(scopes: &[Scope], from: usize, name: &str) -> Option<GroupKey> {
    let scope = &scopes[from];
    if scope.globals.contains(name) {
        return Some((0, name.to_string()));
    }
    if scope.nonlocals.contains(name) {
        let mut parent = scope.parent;
        while let Some(i) = parent {
            if scopes[i].kind == ScopeKind::Function && scopes[i].bindings.contains_key(name) {
                return Some((i, name.to_string()));
            }
            parent = scopes[i].parent;
        }
        return None;
    }
    if scope.bindings.contains_key(name) {
        return Some((from, name.to_string()));
    }
    let mut parent = scope.parent;
    while let Some(i) = parent {
        if scopes[i].kind != ScopeKind::Class && scopes[i].bindings.contains_key(name) {
            return Some((i, name.to_string()));
        }
        parent = scopes[i].parent;
    }
    None
}

// ───────────────────────────────── folding ──────────────────────────────────

// Already a literal: nothing to fold.
fn is_plain_literal(expr: &Expr) -> bool {
    match expr {
        Expr::StringLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::NumberLiteral(_) => true,
        Expr::UnaryOp(u) => {
            matches!(u.op, UnaryOp::USub | UnaryOp::UAdd)
                && matches!(*u.operand, Expr::NumberLiteral(_))
        }
        Expr::List(l) => l.elts.iter().all(is_plain_literal),
        Expr::Tuple(t) => t.elts.iter().all(is_plain_literal),
        Expr::Set(s) => s.elts.iter().all(is_plain_literal),
        Expr::Dict(d) => d
            .items
            .iter()
            .all(|i| i.key.as_ref().is_some_and(is_plain_literal) && is_plain_literal(&i.value)),
        _ => false,
    }
}

const MAX_FOLDED_NODES: usize = 1_000;

fn node_count(value: &Value) -> usize {
    match value {
        Value::List(v) | Value::Tuple(v) | Value::Set(v) => {
            1 + v.iter().map(node_count).sum::<usize>()
        }
        Value::Dict(d) => {
            1 + d
                .iter()
                .map(|(k, v)| node_count(k) + node_count(v))
                .sum::<usize>()
        }
        _ => 1,
    }
}

// The literal expression for `value`, or `None` for one that has no literal form.
fn to_expr(value: &Value, range: TextRange) -> Option<Expr> {
    let node_index = ast::AtomicNodeIndex::default;
    Some(match value {
        Value::Str(s) => Expr::StringLiteral(ast::ExprStringLiteral {
            node_index: node_index(),
            range,
            value: ast::StringLiteralValue::single(ast::StringLiteral {
                range,
                node_index: node_index(),
                value: s.clone().into_boxed_str(),
                flags: ast::StringLiteralFlags::empty(),
            }),
        }),
        Value::Int(i) => {
            let magnitude = Expr::NumberLiteral(ast::ExprNumberLiteral {
                node_index: node_index(),
                range,
                value: ast::Number::Int(ast::Int::from(i.checked_abs()? as u64)),
            });
            if *i < 0 {
                Expr::UnaryOp(ast::ExprUnaryOp {
                    node_index: node_index(),
                    range,
                    op: UnaryOp::USub,
                    operand: Box::new(magnitude),
                })
            } else {
                magnitude
            }
        }
        Value::Bool(b) => Expr::BooleanLiteral(ast::ExprBooleanLiteral {
            node_index: node_index(),
            range,
            value: *b,
        }),
        Value::None => Expr::NoneLiteral(ast::ExprNoneLiteral {
            node_index: node_index(),
            range,
        }),
        Value::List(items) => Expr::List(ast::ExprList {
            node_index: node_index(),
            range,
            elts: items
                .iter()
                .map(|v| to_expr(v, range))
                .collect::<Option<_>>()?,
            ctx: ExprContext::Load,
        }),
        Value::Tuple(items) => Expr::Tuple(ast::ExprTuple {
            node_index: node_index(),
            range,
            elts: items
                .iter()
                .map(|v| to_expr(v, range))
                .collect::<Option<_>>()?,
            ctx: ExprContext::Load,
            parenthesized: true,
        }),
        // `{}` is a dict, so an empty set has no literal form.
        Value::Set(items) if !items.is_empty() => Expr::Set(ast::ExprSet {
            node_index: node_index(),
            range,
            elts: items
                .iter()
                .map(|v| to_expr(v, range))
                .collect::<Option<_>>()?,
        }),
        Value::Set(_) => return None,
        Value::Dict(items) => Expr::Dict(ast::ExprDict {
            node_index: node_index(),
            range,
            items: items
                .iter()
                .map(|(k, v)| {
                    Some(ast::DictItem {
                        key: Some(to_expr(k, range)?),
                        value: to_expr(v, range)?,
                    })
                })
                .collect::<Option<_>>()?,
        }),
    })
}

// Finds the outermost expressions that evaluate, top-down.
struct SiteFinder<'e> {
    evaluator: &'e Evaluator<'e>,
    sites: HashMap<TextRange, Value>,
    // Bare names being accessed, indexed or called. Replacing one gains nothing (the
    // whole expression was tried first) and risks hiding a variable the checker tracks.
    protected: HashSet<TextRange>,
}

impl SiteFinder<'_> {
    fn protect(&mut self, expr: &Expr) {
        if matches!(expr, Expr::Name(_)) {
            self.protected.insert(expr.range());
        }
    }
}

fn is_load(expr: &Expr) -> bool {
    let ctx = match expr {
        Expr::Name(n) => n.ctx,
        Expr::Attribute(a) => a.ctx,
        Expr::Subscript(s) => s.ctx,
        Expr::List(l) => l.ctx,
        Expr::Tuple(t) => t.ctx,
        Expr::Starred(s) => s.ctx,
        _ => ExprContext::Load,
    };
    ctx == ExprContext::Load
}

impl<'a> Visitor<'a> for SiteFinder<'_> {
    fn visit_annotation(&mut self, _expr: &'a Expr) {}
    fn visit_type_params(&mut self, _type_params: &'a ast::TypeParams) {}
    fn visit_expr(&mut self, expr: &'a Expr) {
        if self.protected.contains(&expr.range()) {
            return;
        }
        if is_load(expr) && !is_plain_literal(expr) {
            if let Some(value) = self.evaluator.eval(expr) {
                if node_count(&value) <= MAX_FOLDED_NODES && to_expr(&value, expr.range()).is_some()
                {
                    self.sites.insert(expr.range(), value);
                    return;
                }
            }
        }
        match expr {
            Expr::Attribute(a) => self.protect(&a.value),
            Expr::Subscript(s) => self.protect(&s.value),
            Expr::Call(c) => self.protect(&c.func),
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}

struct Folder<'s> {
    sites: &'s HashMap<TextRange, Value>,
}

impl Transformer for Folder<'_> {
    fn visit_expr(&self, expr: &mut Expr) {
        if let Some(value) = self.sites.get(&expr.range()) {
            if let Some(literal) = to_expr(value, expr.range()) {
                *expr = literal;
                return;
            }
        }
        ast_transformer::walk_expr(self, expr);
    }
}

// The outermost expressions of `module` that evaluate statically, with their values.
// Empty when the analysis can't be trusted for this file.
fn find_sites(module: &ModModule, imports: &ImportedModules) -> HashMap<TextRange, Value> {
    let analysis = Analysis::build(module, imports);
    if !analysis.complete {
        return HashMap::new();
    }
    let evaluator = Evaluator::new(&analysis);
    let mut finder = SiteFinder {
        evaluator: &evaluator,
        sites: HashMap::new(),
        protected: HashSet::new(),
    };
    for stmt in &module.body {
        finder.visit_stmt(stmt);
    }
    finder.sites
}

/// Replaces every expression that evaluates statically with the literal it equals, keeping
/// source ranges so diagnostics still point at the original text. `None` when nothing
/// changes (or the analysis can't be trusted for this file), so the caller keeps using the
/// original module without paying for a clone.
pub(crate) fn fold_constants(module: &ModModule, imports: &ImportedModules) -> Option<ModModule> {
    let sites = find_sites(module, imports);
    if sites.is_empty() {
        return None;
    }
    let mut folded = module.clone();
    let folder = Folder { sites: &sites };
    for stmt in &mut folded.body {
        folder.visit_stmt(stmt);
    }
    Some(folded)
}

#[cfg(test)]
mod evaluator_tests {
    use super::*;
    use ruff_python_parser::parse_expression;

    // Resolves a handful of fixed names; anything else is a builtin.
    struct Env(HashMap<&'static str, Value>);

    impl NameResolver for Env {
        fn resolve(&self, name: &ast::ExprName) -> Resolved {
            match self.0.get(name.id.as_str()) {
                Some(v) => Resolved::Value(v.clone()),
                None if name.id.as_str().starts_with("unknown") => Resolved::Unknown,
                None => Resolved::Builtin,
            }
        }
    }

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }

    fn strs(items: &[&str]) -> Value {
        Value::List(items.iter().map(|i| s(i)).collect())
    }

    fn eval(source: &str) -> Option<Value> {
        let env = Env(HashMap::from([
            ("BASE", strs(&["id", "amount"])),
            ("NAME", s("orders")),
            ("N", Value::Int(3)),
            (
                "ROWS",
                Value::Dict(vec![(s("x"), Value::List(vec![Value::Int(1)]))]),
            ),
        ]));
        let parsed = parse_expression(source).unwrap();
        Evaluator::new(&env).eval(parsed.expr())
    }

    #[test]
    fn test_should_evaluate_literals_and_displays() {
        assert_eq!(eval("\"a\""), Some(s("a")));
        assert_eq!(
            eval("[1, 2]"),
            Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
        );
        assert_eq!(
            eval("(\"a\", \"b\")"),
            Some(Value::Tuple(vec![s("a"), s("b")]))
        );
        assert_eq!(
            eval("{\"a\": 1}"),
            Some(Value::Dict(vec![(s("a"), Value::Int(1))]))
        );
        assert_eq!(
            eval("{\"a\", \"a\", \"b\"}"),
            Some(Value::Set(vec![s("a"), s("b")]))
        );
        assert_eq!(eval("None"), Some(Value::None));
        assert_eq!(eval("-5"), Some(Value::Int(-5)));
    }

    #[test]
    fn test_should_evaluate_names_through_the_resolver() {
        assert_eq!(eval("BASE"), Some(strs(&["id", "amount"])));
        assert_eq!(eval("unknown_thing"), None);
    }

    #[test]
    fn test_should_concatenate_and_repeat() {
        assert_eq!(eval("BASE + [\"x\"]"), Some(strs(&["id", "amount", "x"])));
        assert_eq!(eval("[\"a\"] * 2"), Some(strs(&["a", "a"])));
        assert_eq!(eval("2 * \"ab\""), Some(s("abab")));
        assert_eq!(
            eval("(\"a\",) + (\"b\",)"),
            Some(Value::Tuple(vec![s("a"), s("b")]))
        );
        assert_eq!(eval("\"a\" + 1"), None);
    }

    #[test]
    fn test_should_spread_into_displays() {
        assert_eq!(eval("[*BASE, \"x\"]"), Some(strs(&["id", "amount", "x"])));
        assert_eq!(
            eval("{**{\"a\": 1}, \"b\": 2}"),
            Some(Value::Dict(vec![
                (s("a"), Value::Int(1)),
                (s("b"), Value::Int(2))
            ]))
        );
        assert_eq!(eval("{**unknown_thing}"), None);
    }

    #[test]
    fn test_should_format_fstrings_of_scalars_only() {
        assert_eq!(eval("f\"{NAME}_{N}\""), Some(s("orders_3")));
        assert_eq!(eval("f\"a{True}{None}\""), Some(s("aTrueNone")));
        assert_eq!(eval("f\"{BASE}\""), None);
        assert_eq!(eval("f\"{N:03d}\""), None);
        assert_eq!(eval("f\"{N!r}\""), None);
        assert_eq!(eval("f\"{N=}\""), None);
    }

    #[test]
    fn test_should_evaluate_comprehensions() {
        assert_eq!(
            eval("[f\"m{i}\" for i in range(3)]"),
            Some(strs(&["m0", "m1", "m2"]))
        );
        assert_eq!(
            eval("[c.upper() for c in BASE if c != \"id\"]"),
            Some(strs(&["AMOUNT"]))
        );
        assert_eq!(
            eval("{c: len(c) for c in BASE}"),
            Some(Value::Dict(vec![
                (s("id"), Value::Int(2)),
                (s("amount"), Value::Int(6))
            ]))
        );
        assert_eq!(
            eval("[(a, b) for a in [1, 2] for b in \"xy\"]").map(|v| match v {
                Value::List(items) => items.len(),
                _ => 0,
            }),
            Some(4)
        );
        assert_eq!(
            eval("[a + b for a, b in zip([\"x\"], [\"y\"])]"),
            Some(strs(&["xy"]))
        );
    }

    #[test]
    fn test_should_scope_a_comprehension_variable_to_its_comprehension() {
        // the outer `N` is still 3 afterwards, and the comprehension's own `N` doesn't leak
        assert_eq!(
            eval("[N for N in [1, 2]] + [N]"),
            Some(Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3)
            ]))
        );
        // the first iterable is evaluated in the enclosing scope
        assert_eq!(
            eval("[N for N in range(N)]")
                .map(|v| v == Value::List(vec![Value::Int(0), Value::Int(1), Value::Int(2)])),
            Some(true)
        );
    }

    #[test]
    fn test_should_evaluate_a_generator_only_as_a_builtin_argument() {
        assert_eq!(
            eval("sorted(c for c in BASE)"),
            Some(strs(&["amount", "id"]))
        );
        assert_eq!(eval("\",\".join(c for c in BASE)"), Some(s("id,amount")));
        assert_eq!(eval("(c for c in BASE)"), None);
    }

    #[test]
    fn test_should_evaluate_builtins() {
        assert_eq!(eval("len(BASE)"), Some(Value::Int(2)));
        assert_eq!(eval("sorted(BASE)"), Some(strs(&["amount", "id"])));
        assert_eq!(
            eval("sorted(BASE, reverse=True)"),
            Some(strs(&["id", "amount"]))
        );
        assert_eq!(eval("list(reversed(BASE))"), Some(strs(&["amount", "id"])));
        assert_eq!(eval("min(BASE)"), Some(s("amount")));
        assert_eq!(eval("max(1, 5, 3)"), Some(Value::Int(5)));
        assert_eq!(eval("sum([1, 2, 3])"), Some(Value::Int(6)));
        assert_eq!(
            eval("list(range(2, 8, 3))"),
            Some(Value::List(vec![Value::Int(2), Value::Int(5)]))
        );
        assert_eq!(eval("str(N)"), Some(s("3")));
        assert_eq!(eval("int(\" 12 \")"), Some(Value::Int(12)));
        assert_eq!(eval("any([0, 1])"), Some(Value::Bool(true)));
        assert_eq!(eval("all([])"), Some(Value::Bool(true)));
        assert_eq!(
            eval("dict(a=1)"),
            Some(Value::Dict(vec![(s("a"), Value::Int(1))]))
        );
        assert_eq!(
            eval("dict([(\"a\", 1)])"),
            Some(Value::Dict(vec![(s("a"), Value::Int(1))]))
        );
        assert_eq!(
            eval("list(enumerate(BASE, start=1))"),
            Some(Value::List(vec![
                Value::Tuple(vec![Value::Int(1), s("id")]),
                Value::Tuple(vec![Value::Int(2), s("amount")]),
            ]))
        );
    }

    #[test]
    fn test_should_refuse_unknown_calls_and_shadowed_builtins() {
        assert_eq!(eval("open(\"f\")"), None);
        assert_eq!(eval("unknown_fn(1)"), None);
        assert_eq!(eval("os.environ"), None);
        // a name the resolver knows about is not the builtin
        assert_eq!(eval("N(1)"), None);
    }

    #[test]
    fn test_should_evaluate_string_methods() {
        assert_eq!(eval("\"a,b\".split(\",\")"), Some(strs(&["a", "b"])));
        assert_eq!(eval("\"a b  c\".split()"), Some(strs(&["a", "b", "c"])));
        assert_eq!(eval("\"a,b,c\".split(\",\")[1]"), Some(s("b")));
        assert_eq!(eval("\",\".join(BASE)"), Some(s("id,amount")));
        assert_eq!(eval("\"Ab\".lower()"), Some(s("ab")));
        assert_eq!(eval("\"hello world\".title()"), Some(s("Hello World")));
        assert_eq!(eval("\"x_y\".replace(\"_\", \" \")"), Some(s("x y")));
        assert_eq!(eval("\"  a \".strip()"), Some(s("a")));
        assert_eq!(eval("\"--a--\".strip(\"-\")"), Some(s("a")));
        assert_eq!(eval("\"abc\".startswith(\"ab\")"), Some(Value::Bool(true)));
        assert_eq!(
            eval("\"abc\".endswith((\"x\", \"bc\"))"),
            Some(Value::Bool(true))
        );
        assert_eq!(eval("\"pre_a\".removeprefix(\"pre_\")"), Some(s("a")));
        assert_eq!(eval("\"a\".bogus()"), None);
        assert_eq!(eval("\"a\".replace(\"\", \"x\")"), None);
    }

    #[test]
    fn test_should_evaluate_str_format() {
        assert_eq!(eval("\"{}_{}\".format(\"a\", 1)"), Some(s("a_1")));
        assert_eq!(eval("\"{1}{0}\".format(\"a\", \"b\")"), Some(s("ba")));
        assert_eq!(eval("\"{n}x\".format(n=NAME)"), Some(s("ordersx")));
        assert_eq!(eval("\"{{}}\".format()"), Some(s("{}")));
        assert_eq!(eval("\"{}\".format()"), None);
        assert_eq!(eval("\"{:>5}\".format(1)"), None);
        assert_eq!(eval("\"{a.b}\".format(a=1)"), None);
    }

    #[test]
    fn test_should_evaluate_dict_methods_and_subscripts() {
        assert_eq!(
            eval("list({\"a\": 1, \"b\": 2}.keys())"),
            Some(strs(&["a", "b"]))
        );
        assert_eq!(eval("{\"a\": 1}.get(\"z\", 7)"), Some(Value::Int(7)));
        assert_eq!(eval("{\"a\": 1}[\"a\"]"), Some(Value::Int(1)));
        assert_eq!(eval("{\"a\": 1}[\"z\"]"), None);
        assert_eq!(eval("BASE[0]"), Some(s("id")));
        assert_eq!(eval("BASE[-1]"), Some(s("amount")));
        assert_eq!(eval("BASE[5]"), None);
        assert_eq!(
            eval("[1, 2, 3, 4][1:3]"),
            Some(Value::List(vec![Value::Int(2), Value::Int(3)]))
        );
        assert_eq!(
            eval("[1, 2, 3][::-1]"),
            Some(Value::List(vec![
                Value::Int(3),
                Value::Int(2),
                Value::Int(1)
            ]))
        );
        assert_eq!(eval("\"abcd\"[1:3]"), Some(s("bc")));
    }

    #[test]
    fn test_should_evaluate_comparisons_and_conditions() {
        assert_eq!(eval("1 < 2 < 3"), Some(Value::Bool(true)));
        assert_eq!(eval("1 < 2 > 5"), Some(Value::Bool(false)));
        assert_eq!(eval("\"id\" in BASE"), Some(Value::Bool(true)));
        assert_eq!(eval("\"z\" not in BASE"), Some(Value::Bool(true)));
        assert_eq!(eval("None is None"), Some(Value::Bool(true)));
        assert_eq!(eval("BASE is None"), Some(Value::Bool(false)));
        assert_eq!(eval("BASE is BASE"), None);
        assert_eq!(eval("[\"a\"] if N > 2 else [\"b\"]"), Some(strs(&["a"])));
        assert_eq!(eval("[] or [\"d\"]"), Some(strs(&["d"])));
        assert_eq!(eval("N and \"x\""), Some(s("x")));
        assert_eq!(eval("not BASE"), Some(Value::Bool(false)));
        assert_eq!(eval("True == 1"), Some(Value::Bool(true)));
    }

    #[test]
    fn test_should_not_evaluate_the_untaken_branch() {
        assert_eq!(
            eval("[\"a\"] if True else unknown_thing"),
            Some(strs(&["a"]))
        );
        assert_eq!(eval("True or unknown_thing"), Some(Value::Bool(true)));
    }

    #[test]
    fn test_should_do_set_operations_but_never_order_a_set() {
        assert_eq!(
            eval("{\"a\", \"b\"} | {\"c\"}"),
            Some(Value::Set(vec![s("a"), s("b"), s("c")]))
        );
        assert_eq!(
            eval("{\"a\", \"b\"} & {\"b\"}"),
            Some(Value::Set(vec![s("b")]))
        );
        assert_eq!(
            eval("{\"a\", \"b\"} - {\"b\"}"),
            Some(Value::Set(vec![s("a")]))
        );
        assert_eq!(eval("sorted({\"b\", \"a\"})"), Some(strs(&["a", "b"])));
        assert_eq!(eval("len({\"a\", \"b\"})"), Some(Value::Int(2)));
        assert_eq!(eval("set([\"a\", \"a\"])"), Some(Value::Set(vec![s("a")])));
        // order-dependent uses are refused
        assert_eq!(eval("list({\"a\", \"b\"})"), None);
        assert_eq!(eval("[c for c in {\"a\"}]"), None);
        assert_eq!(eval("[*{\"a\"}]"), None);
        assert_eq!(eval("{\"a\", \"b\"}[0]"), None);
        // but an unordered consumer is fine
        assert_eq!(
            eval("{c.upper() for c in {\"a\"}}"),
            Some(Value::Set(vec![s("A")]))
        );
    }

    #[test]
    fn test_should_refuse_unhashable_set_elements_and_dict_keys() {
        assert_eq!(eval("{[1]}"), None);
        assert_eq!(eval("{[1]: 2}"), None);
    }

    #[test]
    fn test_should_refuse_what_it_cannot_decide() {
        assert_eq!(eval("1.5"), None);
        assert_eq!(eval("b\"x\""), None);
        assert_eq!(eval("lambda: 1"), None);
        assert_eq!(eval("(x := 1)"), None);
        assert_eq!(eval("5 / 2"), None);
        assert_eq!(eval("\"%s\" % 1"), None);
        assert_eq!(eval("sorted([1, \"a\"])"), None);
        assert_eq!(eval("1 + True"), Some(Value::Int(2)));
    }

    #[test]
    fn test_should_refuse_values_that_cannot_all_be_ordered() {
        assert_eq!(eval("sorted([(0, \"a\"), (1, 2), (0, 5)])"), None);
        assert_eq!(
            eval("sorted([(1, 2), (0, 5)])"),
            Some(Value::List(vec![
                Value::Tuple(vec![Value::Int(0), Value::Int(5)]),
                Value::Tuple(vec![Value::Int(1), Value::Int(2)]),
            ]))
        );
    }

    #[test]
    fn test_should_only_evaluate_one_shot_iterators_where_they_are_consumed() {
        assert_eq!(eval("zip([\"a\"], [\"b\"])"), None);
        assert_eq!(eval("reversed([\"a\"])"), None);
        assert_eq!(eval("enumerate([\"a\"])"), None);
        assert_eq!(
            eval("[x for x in reversed([\"a\", \"b\"])]"),
            Some(strs(&["b", "a"]))
        );
    }

    #[test]
    fn test_should_not_overflow_on_an_extreme_slice_step() {
        assert_eq!(
            eval("[\"a\", \"b\"][0::9223372036854775807]"),
            Some(strs(&["a"]))
        );
        assert_eq!(
            eval("[\"a\", \"b\"][::-9223372036854775807]"),
            Some(strs(&["b"]))
        );
    }

    #[test]
    fn test_should_title_case_around_uncased_letters_like_python() {
        assert_eq!(eval("\"\u{4e2d}a b\".title()"), Some(s("\u{4e2d}A B")));
        assert_eq!(eval("\"stra\u{df}e\".title()"), None);
    }

    #[test]
    fn test_should_stop_at_the_size_and_step_limits() {
        assert_eq!(eval("range(1000000000)"), None);
        assert_eq!(eval("range(10 ** 9)"), None); // `**` itself is unsupported
        assert_eq!(eval("range(20000)"), None);
        assert_eq!(eval("\"a\" * 1000000"), None);
        assert_eq!(eval("[0] * 20000"), None);
        assert_eq!(
            eval("[i for i in range(10000) for j in range(10000)]"),
            None
        );
        assert_eq!(eval("1 + 9223372036854775807"), None);
    }

    #[test]
    fn test_should_report_the_string_list_of_a_column_list_value() {
        assert_eq!(
            strs(&["a", "b"]).as_string_list(),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(Value::List(vec![Value::Int(1)]).as_string_list(), None);
        assert_eq!(s("a").as_string_list(), None);
    }

    #[test]
    fn test_should_tell_flat_values_from_values_with_mutable_elements() {
        assert!(strs(&["a"]).has_immutable_elements());
        assert!(Value::Tuple(vec![Value::Tuple(vec![Value::Int(1)])]).has_immutable_elements());
        assert!(Value::Dict(vec![(s("a"), Value::Int(1))]).has_immutable_elements());
        assert!(!Value::List(vec![strs(&["a"])]).has_immutable_elements());
        assert!(!Value::Dict(vec![(s("a"), strs(&["a"]))]).has_immutable_elements());
    }
}

#[cfg(test)]
mod analysis_tests {
    use super::*;
    use ruff_python_parser::parse_module;

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }

    fn strs(items: &[&str]) -> Value {
        Value::List(items.iter().map(|i| s(i)).collect())
    }

    // Every folded expression as (its source text, its value), in source order.
    fn folds(source: &str) -> Vec<(String, Value)> {
        let module = parse_module(source).unwrap().into_syntax();
        let mut sites: Vec<(TextRange, Value)> = find_sites(&module, &ImportedModules::new())
            .into_iter()
            .collect();
        sites.sort_by_key(|(range, _)| range.start());
        sites
            .into_iter()
            .map(|(range, value)| (source[range].to_string(), value))
            .collect()
    }

    // The value `name` folds to where it is used as `print(name)`, if it does.
    fn used_as(source: &str, name: &str) -> Option<Value> {
        folds(source)
            .into_iter()
            .find(|(text, _)| text == name)
            .map(|(_, v)| v)
    }

    const USE: &str = "\nprint(COLS)\n";

    fn with_print(prefix: &str) -> String {
        format!("{prefix}{USE}")
    }

    #[test]
    fn test_should_fold_a_module_constant_and_a_chain_of_them() {
        let source = "BASE = [\"a\"]\nCOLS = BASE + [\"b\"]\nprint(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), Some(strs(&["a", "b"])));
        // the right-hand side of COLS itself folds too
        assert!(folds(source)
            .iter()
            .any(|(t, v)| t == "BASE + [\"b\"]" && *v == strs(&["a", "b"])));
    }

    #[test]
    fn test_should_fold_a_whole_expression_in_an_argument_position() {
        let source = "print(usecols=[f\"m{i}\" for i in range(2)])\n";
        assert_eq!(
            folds(source),
            vec![(
                "[f\"m{i}\" for i in range(2)]".to_string(),
                strs(&["m0", "m1"])
            )]
        );
    }

    #[test]
    fn test_should_not_fold_a_plain_literal() {
        assert!(folds("print(usecols=[\"a\", \"b\"], n=3, d={\"k\": [1]})\n").is_empty());
    }

    #[test]
    fn test_should_not_fold_assignment_targets_or_annotations() {
        assert!(folds("COLS = []\nCOLS[0] = 1\nx: COLS = 1\n").is_empty());
    }

    #[test]
    fn test_should_resolve_each_function_local_name_in_its_own_scope() {
        let source = "def a():\n    cols = [\"x\"]\n    return print(usecols=cols)\n\n\ndef b():\n    cols = [\"y\"]\n    return print(usecols=cols)\n";
        let found: Vec<Value> = folds(source)
            .into_iter()
            .filter(|(t, _)| t == "cols")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(found, vec![strs(&["x"]), strs(&["y"])]);
    }

    #[test]
    fn test_should_see_a_module_constant_from_a_function_unless_it_is_shadowed() {
        let visible = "COLS = [\"m\"]\n\n\ndef f():\n    return print(usecols=COLS)\n";
        assert_eq!(used_as(visible, "COLS"), Some(strs(&["m"])));

        let shadowed = "COLS = [\"m\"]\n\n\ndef f():\n    COLS = [\"local\"]\n    return print(usecols=COLS)\n";
        let found: Vec<Value> = folds(shadowed)
            .into_iter()
            .filter(|(t, _)| t == "COLS")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(found, vec![strs(&["local"])]);
    }

    #[test]
    fn test_should_not_let_a_class_body_name_leak_into_its_methods() {
        let source = "class C:\n    COLS = [\"c\"]\n\n    def m(self):\n        return print(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), None);
    }

    #[test]
    fn test_should_resolve_a_name_in_a_class_body_from_that_body() {
        let source = "class C:\n    COLS = [\"c\"]\n    other = print(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), Some(strs(&["c"])));
    }

    #[test]
    fn test_should_resolve_through_a_closure() {
        let source = "def outer():\n    cols = [\"o\"]\n\n    def inner():\n        return print(usecols=cols)\n\n    return inner\n";
        assert_eq!(used_as(source, "cols"), Some(strs(&["o"])));
    }

    #[test]
    fn test_should_respect_a_comprehension_variable_with_the_name_of_a_constant() {
        let source = "N = 5\nout = print(x=[N for N in [1, 2]])\n";
        assert_eq!(
            folds(source)
                .iter()
                .find(|(t, _)| t.starts_with("[N for"))
                .map(|(_, v)| v.clone()),
            Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    // ── a second binding of any kind ──

    #[test]
    fn test_should_not_fold_a_name_bound_more_than_once() {
        for extra in [
            "COLS = [\"b\"]",
            "COLS += [\"b\"]",
            "COLS, other = [\"b\"], 1",
            "for COLS in []:\n    pass",
            "with open(p) as COLS:\n    pass",
            "import COLS",
            "from m import COLS",
            "def COLS():\n    pass",
            "class COLS:\n    pass",
            "(COLS := [\"b\"])",
            "del COLS",
            "try:\n    pass\nexcept E as COLS:\n    pass",
            "match x:\n    case COLS:\n        pass",
            "match x:\n    case [*COLS]:\n        pass",
            "match x:\n    case {\"k\": 1, **COLS}:\n        pass",
        ] {
            let source = with_print(&format!("COLS = [\"a\"]\n{extra}"));
            assert_eq!(used_as(&source, "COLS"), None, "{extra}");
        }
    }

    #[test]
    fn test_should_not_fold_a_parameter_or_a_comprehension_target() {
        assert_eq!(used_as("def f(COLS):\n    print(COLS)\n", "COLS"), None);
        assert_eq!(used_as("def f(*COLS):\n    print(COLS)\n", "COLS"), None);
        assert_eq!(used_as("lambda COLS: print(COLS)\n", "COLS"), None);
    }

    #[test]
    fn test_should_not_fold_a_name_rebound_through_global_or_nonlocal() {
        let source =
            "COLS = [\"a\"]\n\n\ndef f():\n    global COLS\n    COLS = [\"b\"]\n\n\nprint(COLS)\n";
        assert_eq!(used_as(source, "COLS"), None);

        let source = "def outer():\n    cols = [\"a\"]\n\n    def inner():\n        nonlocal cols\n        cols = [\"b\"]\n\n    print(cols)\n";
        assert_eq!(used_as(source, "cols"), None);
    }

    #[test]
    fn test_should_fold_a_name_a_function_only_reads_through_global() {
        let source =
            "COLS = [\"a\"]\n\n\ndef f():\n    global COLS\n    return print(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), Some(strs(&["a"])));
    }

    // ── a use that could change it ──

    #[test]
    fn test_should_not_fold_a_name_that_escapes() {
        for use_ in [
            "COLS.append(\"x\")",
            "COLS.extend([\"x\"])",
            "COLS.insert(0, \"x\")",
            "COLS.remove(\"a\")",
            "COLS.pop()",
            "COLS.clear()",
            "COLS.sort()",
            "COLS.reverse()",
            "COLS.update({})",
            "COLS.unknown_method()",
            "COLS[0] = \"x\"",
            "COLS[0:1] = []",
            "del COLS[0]",
            "COLS.attr = 1",
            "alias = COLS",
            "alias = [COLS]",
            "alias = (COLS, 1)",
            "alias = {\"k\": COLS}",
            "alias = {COLS}",
            "alias = COLS if flag else other",
            "alias = COLS or other",
            "alias = (y := COLS)",
            "helper(COLS)",
            "helper(x=COLS)",
            "helper(*COLS)",
            "obj.method(COLS)",
            "obj.attr = COLS",
            "obj[0] = COLS",
            "return_it = lambda: COLS",
            "with COLS:\n    pass",
            "raise COLS",
            "await COLS",
            "yield COLS",
            "COLS.__class__",
            "@COLS\ndef f():\n    pass",
        ] {
            let source = format!("COLS = [\"a\"]\nprint(usecols=COLS)\n{use_}\n");
            assert_eq!(used_as(&source, "COLS"), None, "{use_}");
        }
    }

    #[test]
    fn test_should_not_fold_a_name_returned_or_stored_by_default_argument() {
        assert_eq!(
            used_as("COLS = [\"a\"]\n\n\ndef f():\n    return COLS\n", "COLS"),
            None
        );
        assert_eq!(
            used_as("COLS = [\"a\"]\n\n\ndef f(c=COLS):\n    return c\n", "COLS"),
            None
        );
    }

    #[test]
    fn test_should_not_fold_a_name_mutated_from_inside_a_function() {
        let source =
            "COLS = [\"a\"]\n\n\ndef f():\n    COLS.append(\"x\")\n\n\nprint(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), None);
    }

    #[test]
    fn test_should_fold_a_name_that_is_only_read() {
        for use_ in [
            "len(COLS)",
            "\"a\" in COLS",
            "if COLS:\n    pass",
            "print(COLS)",
            "COLS == other",
            "f\"{COLS}\"",
            "x = COLS[0]",
            "x = COLS[1:]",
            "for c in COLS:\n    pass",
            "x = [c for c in COLS]",
            "a, b = COLS",
            "x = COLS + [\"b\"]",
            "x = [*COLS, \"b\"]",
            "x = sorted(COLS)",
            "x = list(COLS)",
            "x = \",\".join(COLS)",
            "x = COLS.index(\"a\")",
            "x = COLS.count(\"a\")",
            "x = COLS.copy()",
            "pd.DataFrame(rows, columns=COLS)",
            "pd.read_csv(\"f\", usecols=COLS)",
        ] {
            let source = format!("COLS = [\"a\", \"b\"]\n{use_}\nprint(usecols=COLS)\n");
            assert_eq!(used_as(&source, "COLS"), Some(strs(&["a", "b"])), "{use_}");
        }
    }

    #[test]
    fn test_should_let_mutable_elements_be_used_only_in_ways_that_expose_none_of_them() {
        // a dict of lists: `len` and a loader argument expose nothing ...
        for use_ in [
            "len(ROWS)",
            "pd.DataFrame(ROWS)",
            "\"x\" in ROWS",
            "print(ROWS)",
        ] {
            let source = format!("ROWS = {{\"x\": [1, 2]}}\n{use_}\nprint(data=ROWS)\n");
            assert!(used_as(&source, "ROWS").is_some(), "{use_}");
        }
        // ... but anything that hands out an inner list lets it be changed unseen
        for use_ in [
            "for k in ROWS:\n    pass",
            "x = ROWS[\"x\"]",
            "x = ROWS.values()",
            "x = ROWS.items()",
            "x = ROWS.get(\"x\")",
            "x = dict(ROWS)",
            "x = ROWS.copy()",
            "x = {**ROWS}",
            "x = [ROWS]",
            "x = [c for c in ROWS]",
        ] {
            let source = format!("ROWS = {{\"x\": [1, 2]}}\n{use_}\nprint(data=ROWS)\n");
            assert_eq!(used_as(&source, "ROWS"), None, "{use_}");
        }
    }

    #[test]
    fn test_should_treat_a_list_of_dicts_the_same_way() {
        let flat = "ROWS = [{\"x\": 1}]\npd.DataFrame(ROWS)\nlen(ROWS)\n";
        assert!(folds(flat).iter().any(|(t, _)| t == "ROWS"));
        let exposed = "ROWS = [{\"x\": 1}]\nfirst = ROWS[0]\npd.DataFrame(ROWS)\n";
        assert!(!folds(exposed).iter().any(|(t, _)| t == "ROWS"));
    }

    #[test]
    fn test_should_not_trust_a_builtin_the_file_rebinds() {
        // `len` is rebound, so `len(COLS)` may do anything to COLS
        let source = "COLS = [\"a\"]\n\n\ndef len(x):\n    x.append(1)\n\n\nlen(COLS)\nprint(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), None);
        // and a rebound builtin is not evaluated as the builtin: it is the file's own function
        let source = "def len(x):\n    return 99\n\n\nprint(n=len([1]))\n";
        assert_eq!(
            folds(source),
            vec![("len([1])".to_string(), Value::Int(99))]
        );
    }

    #[test]
    fn test_should_not_trust_a_loader_name_the_file_defines_itself() {
        let own = "COLS = [\"a\"]\n\n\ndef read_csv(path, usecols):\n    usecols.append(1)\n\n\nread_csv(\"f\", usecols=COLS)\n";
        assert_eq!(used_as(own, "COLS"), None);
        let library = "COLS = [\"a\"]\nread_csv(\"f\", usecols=COLS)\n";
        assert_eq!(used_as(library, "COLS"), Some(strs(&["a"])));
    }

    #[test]
    fn test_should_not_fold_module_names_when_the_namespace_can_change_unseen() {
        for hazard in [
            "globals()[\"COLS\"] = []",
            "vars()[\"COLS\"] = []",
            "exec(code)",
            "eval(code)",
            "locals()[\"COLS\"] = []",
            "from other import *",
        ] {
            let source = format!("COLS = [\"a\"]\n{hazard}\nprint(usecols=COLS)\n");
            assert_eq!(used_as(&source, "COLS"), None, "{hazard}");
        }
        // function locals are not affected by `globals()`
        let source =
            "def f():\n    cols = [\"a\"]\n    return print(usecols=cols)\n\n\nglobals()\n";
        assert_eq!(used_as(source, "cols"), Some(strs(&["a"])));
    }

    #[test]
    fn test_should_fold_a_constant_with_a_plain_type_annotation() {
        for annotation in [
            "list[str]",
            "List[str]",
            "typing.List[str]",
            "Final",
            "Final[list[str]]",
            "Sequence[str]",
            "tuple[str, ...]",
            "Optional[list[str]]",
            "list[str] | None",
        ] {
            let source = format!("COLS: {annotation} = [\"a\"]\nprint(COLS)\n");
            assert_eq!(used_as(&source, "COLS"), Some(strs(&["a"])), "{annotation}");
        }
    }

    #[test]
    fn test_should_not_fold_a_name_whose_annotation_says_it_is_something_else() {
        // `df` is a DataFrame to the checker even though its value here is `None`
        for annotation in [
            "Annotated[pl.DataFrame, Schema]",
            "pd.DataFrame",
            "DataFrame[Schema]",
            "Schema",
            "\"list[str]\"",
        ] {
            let source = format!("df: {annotation} = None\nprint(df)\n");
            assert_eq!(used_as(&source, "df"), None, "{annotation}");
        }
    }

    #[test]
    fn test_should_not_replace_a_name_that_is_accessed_indexed_or_called() {
        // evaluating the whole expression fails (unknown method / key), and the name
        // itself must still be left alone
        for use_ in ["NAME.unknown()", "NAME.attr", "NAME[idx]", "NAME(1)"] {
            let source = format!("NAME = \"orders\"\nx = {use_}\n");
            assert!(folds(&source).iter().all(|(t, _)| t != "NAME"), "{use_}");
        }
    }

    #[test]
    fn test_should_still_fold_a_name_used_as_an_argument_or_index() {
        let source = "NAME = \"orders\"\nx = f(NAME)\ny = z[NAME]\n";
        assert_eq!(folds(source).iter().filter(|(t, _)| t == "NAME").count(), 2);
    }

    #[test]
    fn test_should_stay_complete_on_a_type_alias_statement() {
        // a `type X = ...` alias is type-level, so it must not make the analysis give up
        let source = "type Alias = list[str]\nCOLS = [\"a\"]\nprint(COLS)\n";
        let module = parse_module(source).unwrap().into_syntax();
        assert!(Analysis::build(&module, &ImportedModules::new()).complete);
        assert_eq!(used_as(source, "COLS"), Some(strs(&["a"])));
    }

    // ── stage 2: functions and class attributes ──

    // The value a whole expression folds to, found by its source text.
    fn folded(source: &str, text: &str) -> Option<Value> {
        folds(source)
            .into_iter()
            .find(|(t, _)| t == text)
            .map(|(_, v)| v)
    }

    #[test]
    fn test_should_not_hand_a_one_shot_iterator_to_a_user_function() {
        let source = "A = [\"a\", \"b\"]\ndef f(it):\n    return list(it) + list(it)\n";
        for call in [
            "f(x for x in A)",
            "f(zip(A, A))",
            "len(zip(A, A))",
            "len(reversed(A))",
            "bool(zip(A, A))",
            "list(reversed(zip(A, A)))",
        ] {
            let source = format!("{source}print({call})\n");

            assert_eq!(folded(&source, call), None, "{call}");
        }
        let ok = format!("{source}print(f([\"x\"]))\n");
        assert_eq!(folded(&ok, "f([\"x\"])"), Some(strs(&["x", "x"])));
    }

    #[test]
    fn test_should_evaluate_a_call_to_a_function_that_returns_a_constant() {
        let source = "def cols():\n    return [\"a\", \"b\"]\n\n\nprint(usecols=cols())\n";
        assert_eq!(folded(source, "cols()"), Some(strs(&["a", "b"])));
    }

    #[test]
    fn test_should_pass_arguments_by_position_by_keyword_and_by_default() {
        let def =
            "def cols(prefix=\"m\", n=2):\n    return [f\"{prefix}{i}\" for i in range(n)]\n\n\n";
        for (call, expected) in [
            ("cols()", strs(&["m0", "m1"])),
            ("cols(\"z\")", strs(&["z0", "z1"])),
            ("cols(n=1)", strs(&["m0"])),
            ("cols(prefix=\"q\", n=3)", strs(&["q0", "q1", "q2"])),
            ("cols(\"q\", 1)", strs(&["q0"])),
        ] {
            let source = format!("{def}print(usecols={call})\n");
            assert_eq!(folded(&source, call), Some(expected), "{call}");
        }
    }

    #[test]
    fn test_should_run_local_assignments_a_docstring_and_annotations() {
        let source = "def cols() -> list[str]:\n    \"\"\"Doc.\"\"\"\n    base: list[str] = [\"a\"]\n    extra = [\"b\"]\n    base = base + extra\n    return base\n\n\nprint(usecols=cols())\n";
        assert_eq!(folded(source, "cols()"), Some(strs(&["a", "b"])));
    }

    #[test]
    fn test_should_read_module_constants_and_call_other_functions() {
        let source = "BASE = [\"a\"]\n\n\ndef base():\n    return BASE + [\"b\"]\n\n\ndef cols():\n    return base() + [\"c\"]\n\n\nprint(usecols=cols())\n";
        assert_eq!(folded(source, "cols()"), Some(strs(&["a", "b", "c"])));
    }

    #[test]
    fn test_should_give_each_call_a_fresh_default() {
        let source = "def f(x=[]):\n    return x + [\"a\"]\n\n\nprint(a=f(), b=f())\n";
        let found: Vec<Value> = folds(source)
            .into_iter()
            .filter(|(t, _)| t == "f()")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(found, vec![strs(&["a"]), strs(&["a"])]);
    }

    #[test]
    fn test_should_let_a_local_shadow_a_module_constant() {
        let source = "COLS = [\"module\"]\n\n\ndef f():\n    COLS = [\"local\"]\n    return COLS\n\n\nprint(a=f())\n";
        assert_eq!(folded(source, "f()"), Some(strs(&["local"])));
    }

    #[test]
    fn test_should_refuse_a_function_it_cannot_evaluate() {
        for def in [
            // control flow and statements that do anything but assign and return
            "def f():\n    for i in range(2):\n        pass\n    return [\"a\"]",
            "def f():\n    if True:\n        return [\"a\"]\n    return [\"b\"]",
            "def f():\n    while False:\n        pass\n    return [\"a\"]",
            "def f():\n    print(\"x\")\n    return [\"a\"]",
            "def f():\n    x = [\"a\"]\n    x.append(\"b\")\n    return x",
            "def f():\n    try:\n        return [\"a\"]\n    except Exception:\n        return []",
            "def f():\n    with open(\"p\") as h:\n        return [\"a\"]",
            "def f():\n    global G\n    return [\"a\"]",
            "def f():\n    a, b = [1, 2]\n    return [a]",
            "def f():\n    x = [\"a\"]",
            "def f():\n    return [\"a\"]\n    return [\"b\"]",
            "def f():\n    pass",
            // side effects and runtime state
            "def f():\n    return open(\"p\").read().split()",
            "def f():\n    return list(os.environ)",
            "def f():\n    return [g()]",
            // shapes it does not model
            "@cache\ndef f():\n    return [\"a\"]",
            "async def f():\n    return [\"a\"]",
            "def f():\n    yield \"a\"",
            "def f(*args):\n    return [\"a\"]",
            "def f(**kwargs):\n    return [\"a\"]",
            "def f():\n    return f()",
        ] {
            let source = format!("{def}\n\n\nprint(usecols=f())\n");
            assert_eq!(folded(&source, "f()"), None, "{def}");
        }
    }

    #[test]
    fn test_should_refuse_a_call_with_the_wrong_arguments() {
        let def = "def f(a, b=1, *, c=2):\n    return [a, b, c]\n\n\n";
        for call in [
            "f()",
            "f(1, 2, 3)",
            "f(1, d=4)",
            "f(1, a=2)",
            "f(*[1])",
            "f(**{\"a\": 1})",
            "f(unknown)",
            "f(g())",
        ] {
            let source = format!("{def}print(x={call})\n");
            assert_eq!(folded(&source, call), None, "{call}");
        }
        let ok = "def f(a, b=1, *, c=2):\n    return [a, b, c]\n\n\nprint(x=f(0, c=9))\n";
        assert_eq!(
            folded(ok, "f(0, c=9)"),
            Some(Value::List(vec![
                Value::Int(0),
                Value::Int(1),
                Value::Int(9)
            ]))
        );
    }

    #[test]
    fn test_should_refuse_a_function_that_is_defined_twice_or_not_at_all() {
        let twice =
            "def f():\n    return [\"a\"]\n\n\ndef f():\n    return [\"b\"]\n\n\nprint(x=f())\n";
        assert_eq!(folded(twice, "f()"), None);
        assert_eq!(folded("print(x=f())\n", "f()"), None);
        let reassigned = "def f():\n    return [\"a\"]\n\n\nf = other\nprint(x=f())\n";
        assert_eq!(folded(reassigned, "f()"), None);
    }

    #[test]
    fn test_should_refuse_mutual_recursion_and_stop_at_a_call_limit() {
        let mutual = "def a():\n    return b()\n\n\ndef b():\n    return a()\n\n\nprint(x=a())\n";
        assert_eq!(folded(mutual, "a()"), None);
        let chain: String = (0..40)
            .map(|i| format!("def f{i}():\n    return f{}()\n\n\n", i + 1))
            .collect::<String>()
            + "def f40():\n    return [\"end\"]\n\n\nprint(x=f0())\n";
        assert_eq!(folded(&chain, "f0()"), None);
    }

    #[test]
    fn test_should_not_trust_a_function_when_the_module_namespace_can_change() {
        let source = "def f():\n    return [\"a\"]\n\n\nglobals()\nprint(x=f())\n";
        assert_eq!(folded(source, "f()"), None);
    }

    #[test]
    fn test_should_not_let_a_list_escape_through_a_function_it_is_passed_to() {
        // `ident` returns its argument, so ALIAS would be COLS itself
        let source = "COLS = [\"a\"]\n\n\ndef ident(x):\n    return x\n\n\nALIAS = ident(COLS)\nprint(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), None);
        // an immutable argument is fine
        let source = "NAME = \"a\"\n\n\ndef ident(x):\n    return x\n\n\nprint(a=ident(NAME))\n";
        assert_eq!(folded(source, "ident(NAME)"), Some(s("a")));
    }

    #[test]
    fn test_should_read_a_class_attribute_by_class_name() {
        let source = "class Cols:\n    NAMES = [\"a\", \"b\"]\n\n\nprint(usecols=Cols.NAMES)\n";
        assert_eq!(folded(source, "Cols.NAMES"), Some(strs(&["a", "b"])));
    }

    #[test]
    fn test_should_read_an_annotated_class_attribute_and_one_built_from_another() {
        let source = "from typing import ClassVar\n\n\nclass C:\n    A: ClassVar[list[str]] = [\"a\"]\n    B = A + [\"b\"]\n\n\nprint(usecols=C.B)\n";
        assert_eq!(folded(source, "C.B"), Some(strs(&["a", "b"])));
    }

    #[test]
    fn test_should_inherit_a_class_attribute_from_a_single_user_defined_base() {
        let source = "class Base:\n    X = [\"a\"]\n    Y = [\"base\"]\n\n\nclass Child(Base):\n    Y = [\"child\"]\n\n\nprint(a=Child.X, b=Child.Y)\n";
        assert_eq!(folded(source, "Child.X"), Some(strs(&["a"])));
        assert_eq!(folded(source, "Child.Y"), Some(strs(&["child"])));
    }

    #[test]
    fn test_should_refuse_a_class_it_cannot_vouch_for() {
        for header in [
            "class Cols(Enum):",
            "class Cols(BaseSchema):",
            "class Cols(Base1, Base2):",
            "class Cols(metaclass=Meta):",
            "class Cols(Imported):",
            "@dataclass\nclass Cols:",
        ] {
            let source = format!("{header}\n    NAMES = [\"a\"]\n\n\nprint(usecols=Cols.NAMES)\n");
            assert_eq!(folded(&source, "Cols.NAMES"), None, "{header}");
        }
    }

    #[test]
    fn test_should_refuse_a_class_attribute_that_can_change() {
        for change in [
            "Cols.NAMES = [\"z\"]",
            "Cols.NAMES += [\"z\"]",
            "del Cols.NAMES",
            "Cols.NAMES.append(\"z\")",
            "Cols.NAMES[0] = \"z\"",
            "setattr(Cols, \"NAMES\", [])",
            "delattr(Cols, \"NAMES\")",
            "vars(Cols)",
            "alias = Cols.NAMES",
            "helper(Cols.NAMES)",
        ] {
            let source = format!(
                "class Cols:\n    NAMES = [\"a\"]\n\n\n{change}\nprint(usecols=Cols.NAMES)\n"
            );
            assert_eq!(folded(&source, "Cols.NAMES"), None, "{change}");
        }
    }

    #[test]
    fn test_should_refuse_an_attribute_the_class_does_not_define_once() {
        let twice = "class Cols:\n    NAMES = [\"a\"]\n    NAMES = [\"b\"]\n\n\nprint(usecols=Cols.NAMES)\n";
        assert_eq!(folded(twice, "Cols.NAMES"), None);
        let missing = "class Cols:\n    NAMES = [\"a\"]\n\n\nprint(usecols=Cols.OTHER)\n";
        assert_eq!(folded(missing, "Cols.OTHER"), None);
        let rebound =
            "class Cols:\n    NAMES = [\"a\"]\n\n\nCols = Other\nprint(usecols=Cols.NAMES)\n";
        assert_eq!(folded(rebound, "Cols.NAMES"), None);
    }

    #[test]
    fn test_should_not_read_a_class_attribute_through_self_or_an_instance_or_nesting() {
        let source = "class Cols:\n    NAMES = [\"a\"]\n\n    def m(self):\n        return self.NAMES\n\n\nc = Cols()\nprint(a=c.NAMES)\n";
        assert_eq!(folded(source, "c.NAMES"), None);
        let nested =
            "class Outer:\n    class Inner:\n        X = [\"a\"]\n\n\nprint(a=Outer.Inner.X)\n";
        assert_eq!(folded(nested, "Outer.Inner.X"), None);
    }

    #[test]
    fn test_should_not_call_a_method_through_its_class() {
        let source = "class C:\n    def m(self):\n        return [\"a\"]\n\n\nprint(a=C.m())\n";
        assert_eq!(folded(source, "C.m()"), None);
    }

    #[test]
    fn test_should_leave_a_name_that_is_also_a_class_attribute_independent() {
        // `NAMES` at module level and `Cols.NAMES` are different variables
        let source = "NAMES = [\"module\"]\n\n\nclass Cols:\n    NAMES = [\"class\"]\n\n\nprint(a=NAMES, b=Cols.NAMES)\n";
        assert_eq!(folded(source, "NAMES"), Some(strs(&["module"])));
        assert_eq!(folded(source, "Cols.NAMES"), Some(strs(&["class"])));
    }

    #[test]
    fn test_should_not_fold_a_cyclic_definition() {
        assert_eq!(used_as("A = B\nB = A\nprint(A)\n", "A"), None);
        assert_eq!(used_as("A = A + [1]\nprint(A)\n", "A"), None);
    }

    #[test]
    fn test_should_not_fold_what_depends_on_runtime_state() {
        for rhs in [
            "open(p).read().split()",
            "json.print(f)",
            "os.environ[\"X\"].split(\",\")",
            "sys.argv[1:]",
            "random.sample([\"a\", \"b\"], 1)",
            "[x for x in get_cols()]",
            "list(df.columns)",
            "other_module.COLS",
            "param_default",
        ] {
            let source = format!("COLS = {rhs}\nprint(usecols=COLS)\n");
            assert_eq!(used_as(&source, "COLS"), None, "{rhs}");
        }
    }

    #[test]
    fn test_should_fold_into_a_conditional_expression_and_a_comparison() {
        let source = "DEBUG = False\nCOLS = [\"a\"] if DEBUG else [\"b\"]\nprint(usecols=COLS)\n";
        assert_eq!(used_as(source, "COLS"), Some(strs(&["b"])));
    }

    #[test]
    fn test_should_fold_a_dict_and_its_keys() {
        let source =
            "D = {\"a\": 1, \"b\": 2}\nprint(usecols=list(D))\nprint(usecols=sorted(D.keys()))\n";
        let folded = folds(source);
        assert!(folded
            .iter()
            .any(|(t, v)| t == "list(D)" && *v == strs(&["a", "b"])));
        assert!(folded
            .iter()
            .any(|(t, v)| t == "sorted(D.keys())" && *v == strs(&["a", "b"])));
    }

    #[test]
    fn test_should_leave_a_use_inside_an_f_string_foldable() {
        let source = "TABLE = \"orders\"\nprint(sql=f\"select * from {TABLE}\")\n";
        assert!(folds(source)
            .iter()
            .any(|(t, v)| t == "f\"select * from {TABLE}\"" && *v == s("select * from orders")));
    }

    // ── the fold itself ──

    fn folded_value_of_last_statement(source: &str) -> Option<Value> {
        struct NoNames;
        impl NameResolver for NoNames {
            fn resolve(&self, _: &ast::ExprName) -> Resolved {
                Resolved::Unknown
            }
        }
        let module = parse_module(source).unwrap().into_syntax();
        let folded = fold_constants(&module, &ImportedModules::new())?;
        let ast::Stmt::Assign(assign) = folded.body.last()? else {
            return None;
        };
        // evaluating the replacement with NO names proves it is a self-contained literal
        Evaluator::new(&NoNames).eval(&assign.value)
    }

    #[test]
    fn test_should_replace_with_a_self_contained_literal_of_the_same_value() {
        // (setup, right-hand side, value it must equal once folded)
        let cases: Vec<(&str, &str, Value)> = vec![
            ("BASE = [\"a\"]", "BASE + [\"b\"]", strs(&["a", "b"])),
            (
                "BASE = (\"a\",)",
                "{\"k\": BASE}",
                Value::Dict(vec![(s("k"), Value::Tuple(vec![s("a")]))]),
            ),
            (
                "BASE = [\"a\"]",
                "(BASE[0], -3, True, None)",
                Value::Tuple(vec![s("a"), Value::Int(-3), Value::Bool(true), Value::None]),
            ),
            ("BASE = [\"a\"]", "{BASE[0]}", Value::Set(vec![s("a")])),
            ("BASE = [\"a\"]", "f\"x_{BASE[0]}\"", s("x_a")),
        ];
        for (setup, rhs, expected) in cases {
            let source = format!("{setup}\nOUT = {rhs}\n");
            assert_eq!(
                folded_value_of_last_statement(&source),
                Some(expected),
                "{rhs}"
            );
        }
    }

    #[test]
    fn test_should_let_a_value_nothing_can_change_be_used_any_way_at_all() {
        // strings, ints and tuples of them can't be mutated through any alias
        for binding in [
            "NAME = \"orders\"",
            "NAME = 3",
            "NAME = (\"a\", 1)",
            "NAME = None",
        ] {
            for use_ in [
                "alias = NAME",
                "helper(NAME)",
                "other = [NAME]",
                "other = {\"k\": NAME}",
                "obj.attr = NAME",
                "x = NAME if flag else 0",
            ] {
                let source = format!("{binding}\n{use_}\nprint(NAME)\n");
                assert!(
                    folds(&source).iter().any(|(t, _)| t == "NAME"),
                    "{binding}: {use_}"
                );
            }
        }
    }

    #[test]
    fn test_should_never_let_a_container_be_aliased_or_stored() {
        for binding in [
            "NAME = [\"a\"]",
            "NAME = {\"a\": 1}",
            "NAME = {\"a\"}",
            "NAME = ([\"a\"], 1)",
        ] {
            for use_ in [
                "alias = NAME",
                "helper(NAME)",
                "other = [NAME]",
                "other = {\"k\": NAME}",
            ] {
                let source = format!("{binding}\n{use_}\nprint(NAME)\n");
                assert!(
                    !folds(&source).iter().any(|(t, _)| t == "NAME"),
                    "{binding}: {use_}"
                );
            }
        }
    }

    #[test]
    fn test_should_keep_the_original_source_range_of_a_folded_expression() {
        let source = "BASE = [\"a\"]\nOUT = BASE + [\"b\"]\n";
        let module = parse_module(source).unwrap().into_syntax();
        let folded = fold_constants(&module, &ImportedModules::new()).unwrap();
        let (ast::Stmt::Assign(original), ast::Stmt::Assign(new)) =
            (&module.body[1], &folded.body[1])
        else {
            panic!("expected assignments");
        };
        assert_eq!(original.value.range(), new.value.range());
        assert_eq!(
            new.value.range(),
            original.range().add_start(TextSize::from(6))
        );
    }

    #[test]
    fn test_should_return_none_when_nothing_folds() {
        let module = parse_module("x = [\"a\"]\nprint(usecols=x2)\n")
            .unwrap()
            .into_syntax();
        assert!(fold_constants(&module, &ImportedModules::new()).is_none());
    }

    #[test]
    fn test_should_not_fold_a_folded_literal_larger_than_the_cap() {
        let source = "BIG = list(range(2000))\nprint(x=BIG)\n";
        assert!(folds(source).iter().all(|(t, _)| t != "BIG"));
    }

    #[test]
    fn test_should_not_fold_an_empty_set_which_has_no_literal_form() {
        assert!(folds("E = set()\nprint(x=E)\n")
            .iter()
            .all(|(t, _)| t != "E"));
    }
}
