use super::*;

impl<'r> Evaluator<'r> {
    pub(super) fn builtin(&self, name: &str, call: &ast::ExprCall) -> Option<Value> {
        let ordered = !matches!(
            name,
            "set" | "frozenset" | "sorted" | "min" | "max" | "sum" | "any" | "all"
        );
        match name {
            "len" => self.builtin_len(call),
            "list" | "tuple" => self.builtin_list_tuple(name, call, ordered),
            "set" | "frozenset" => self.builtin_set_frozenset(call),
            "sorted" => self.builtin_sorted(call),
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
            "range" => self.builtin_range(call),
            "zip" => self.builtin_zip(call),
            "enumerate" => self.builtin_enumerate(call),
            "min" | "max" => self.builtin_min_max(name, call),
            "sum" => self.builtin_sum(call),
            "dict" | "frozendict" => self.builtin_dict_frozendict(call),
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
            "any" | "all" => self.builtin_all(name, call),
            _ => None,
        }
    }

    fn builtin_zip(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_all(&self, name: &str, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_sum(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_list_tuple(
        &self,
        name: &str,
        call: &ruff_python_ast::ExprCall,
        ordered: bool,
    ) -> Option<Value> {
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

    fn builtin_len(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_set_frozenset(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_min_max(&self, name: &str, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_range(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_enumerate(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_sorted(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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

    fn builtin_dict_frozendict(&self, call: &ruff_python_ast::ExprCall) -> Option<Value> {
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
}
