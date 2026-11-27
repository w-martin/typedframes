use super::*;

impl<'r> Evaluator<'r> {
    pub(super) fn method(
        &self,
        receiver: Value,
        name: &str,
        call: &ast::ExprCall,
    ) -> Option<Value> {
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
            ("capitalize", []) => str_method_capitalize(s),
            ("title", []) => str_method_title(s),
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
                str_affix_test(s, name, text, prefix)
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

fn str_method_capitalize(s: &str) -> Option<Value> {
    let mut chars = s.chars();
    let first = chars.next()?;
    Some(Value::Str(format!(
        "{}{}",
        first.to_uppercase(),
        chars.as_str().to_lowercase()
    )))
}

fn str_affix_test(
    s: &str,
    name: &str,
    text: impl Fn(&Value) -> Option<String>,
    prefix: &Value,
) -> Option<Value> {
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

fn str_method_title(s: &str) -> Option<Value> {
    let mut out = String::new();
    let mut in_word = false;
    for c in s.chars() {
        if c.is_lowercase() || c.is_uppercase() {
            // Titlecase digraphs and ß map to more than one character.
            (c.to_uppercase().count() == 1 && c.to_lowercase().count() == 1).then_some(())?;
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

#[derive(Clone, Copy)]
pub(super) enum DisplayKind {
    Ordered,
    Unordered,
}

// How `str()`/an f-string interpolation prints a scalar. Containers are not handled:
// their repr has quoting rules that are easy to get subtly wrong.
pub(super) fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Bool(true) => Some("True".to_string()),
        Value::Bool(false) => Some("False".to_string()),
        Value::None => Some("None".to_string()),
        _ => None,
    }
}

pub(super) fn contains(container: &Value, item: &Value) -> Option<bool> {
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

pub(super) fn none_identity(left: &Value, right: &Value) -> Option<bool> {
    match (left, right) {
        (Value::None, Value::None) => Some(true),
        (Value::None, _) | (_, Value::None) => Some(false),
        _ => None,
    }
}

pub(super) fn normalize_index(index: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).ok()?;
    let resolved = if index < 0 { index + len } else { index };
    (0..len)
        .contains(&resolved)
        .then(|| usize::try_from(resolved).ok())?
}

// How a sliced sequence's picked elements become a value of the same kind again.
type Rebuild = fn(Vec<Value>) -> Value;

pub(super) fn slice_value(
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
