use super::*;

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

/// Evaluates expressions that read one frame's `.columns` (`{c: c.lower() for c in
/// df.columns}`) given that frame's known column list. Every other name is treated as a
/// builtin, which only a call to a recognized builtin can use.
pub(crate) struct FrameColumns<'a> {
    pub(crate) frame: &'a str,
    pub(crate) columns: &'a [String],
}

impl NameResolver for FrameColumns<'_> {
    fn resolve(&self, name: &ast::ExprName) -> Resolved {
        if name.id.as_str() == self.frame {
            Resolved::Unknown
        } else {
            Resolved::Builtin
        }
    }

    fn resolve_attribute(&self, attribute: &ast::ExprAttribute) -> Resolved {
        match &*attribute.value {
            Expr::Name(n)
                if n.id.as_str() == self.frame && attribute.attr.as_str() == "columns" =>
            {
                Resolved::Value(Value::List(
                    self.columns.iter().cloned().map(Value::Str).collect(),
                ))
            }
            _ => Resolved::Unknown,
        }
    }
}

/// Evaluates an expression over loop variables bound to known values; every other name is a
/// builtin, which only a call to a recognized builtin can use.
pub(crate) struct LoopNames<'a> {
    pub(crate) bound: &'a HashMap<String, Value>,
}

impl NameResolver for LoopNames<'_> {
    fn resolve(&self, name: &ast::ExprName) -> Resolved {
        match self.bound.get(name.id.as_str()) {
            Some(value) => Resolved::Value(value.clone()),
            None => Resolved::Builtin,
        }
    }
}

pub(super) struct DepthGuard<'e>(pub(super) &'e Cell<u32>);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
