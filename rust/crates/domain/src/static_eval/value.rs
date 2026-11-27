use super::*;

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

    /// The value holds a string somewhere, so it could be (or build) a column name. A number,
    /// a flag or `None` cannot, and a tuning constant read through `self` is not worth a
    /// notice about what a subclass might rebind.
    pub(crate) fn can_name_columns(&self) -> bool {
        match self {
            Value::Str(_) => true,
            Value::Int(_) | Value::Bool(_) | Value::None => false,
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => {
                v.iter().any(Value::can_name_columns)
            }
            Value::Dict(items) => items
                .iter()
                .any(|(k, v)| k.can_name_columns() || v.can_name_columns()),
        }
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
        match self {
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => v.iter().all(scalar),
            Value::Dict(items) => items.iter().all(|(k, v)| scalar(k) && scalar(v)),
            _ => true,
        }
    }

    pub(super) fn truthy(&self) -> bool {
        match self {
            Value::Str(s) => !s.is_empty(),
            Value::Int(i) => *i != 0,
            Value::Bool(b) => *b,
            Value::None => false,
            Value::List(v) | Value::Tuple(v) | Value::Set(v) => !v.is_empty(),
            Value::Dict(d) => !d.is_empty(),
        }
    }

    pub(super) fn hashable(&self) -> bool {
        match self {
            Value::Str(_) | Value::Int(_) | Value::Bool(_) | Value::None => true,
            Value::Tuple(items) => items.iter().all(Value::hashable),
            _ => false,
        }
    }

    // `True == 1` in Python, so numbers compare across `Int`/`Bool`.
    pub(super) fn number(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub(super) fn py_eq(&self, other: &Value) -> bool {
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

    pub(super) fn compare(&self, other: &Value) -> Option<Ordering> {
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

pub(super) fn scalar(v: &Value) -> bool {
    match v {
        Value::Str(_) | Value::Int(_) | Value::Bool(_) | Value::None => true,
        Value::Tuple(items) => items.iter().all(scalar),
        _ => false,
    }
}

pub(super) fn set_insert(items: &mut Vec<Value>, value: Value) {
    if !items.iter().any(|v| v.py_eq(&value)) {
        items.push(value);
    }
}

pub(super) fn dict_insert(items: &mut Vec<(Value, Value)>, key: Value, value: Value) {
    match items.iter_mut().find(|(k, _)| k.py_eq(&key)) {
        Some(entry) => entry.1 = value,
        None => items.push((key, value)),
    }
}
