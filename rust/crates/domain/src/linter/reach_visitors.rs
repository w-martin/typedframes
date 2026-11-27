use super::*;

pub(super) fn collect<'e>(target: &'e Expr, direct: bool, out: &mut Vec<(&'e str, bool)>) {
    match target {
        Expr::Name(name) => out.push((name.id.as_str(), direct)),
        Expr::Tuple(tuple) => tuple.elts.iter().for_each(|e| collect(e, false, out)),
        Expr::List(list) => list.elts.iter().for_each(|e| collect(e, false, out)),
        Expr::Starred(starred) => collect(&starred.value, false, out),
        _ => {}
    }
}

pub(super) struct ReachesSelf<'f> {
    pub(super) linter: &'f Linter,
    pub(super) reaches: bool,
    pub(super) call: bool,
    pub(super) self_name: bool,
}

impl ReachesSelf<'_> {
    fn harmless_call(&self, func: &Expr) -> bool {
        let mut root = match func {
            Expr::Name(n) => return HARMLESS_CALLEES.contains(&n.id.as_str()),
            Expr::Attribute(a) => &*a.value,
            _ => return false,
        };
        loop {
            root = match root {
                Expr::Attribute(a) => &a.value,
                Expr::Call(c) => &c.func,
                Expr::Subscript(s) => &s.value,
                Expr::StringLiteral(_) => return true,
                Expr::Name(n) => {
                    let n = n.id.as_str();
                    return n != "self"
                        && (LOAD_MODULES.contains(&n)
                            || HARMLESS_MODULES.contains(&n)
                            || self.linter.variables.contains_key(n)
                            || self.linter.open_frames.contains_key(n));
                }
                _ => return false,
            };
        }
    }

    fn note_call(&mut self, call: &ruff_python_ast::ExprCall) {
        self.call = true;
        if !self.harmless_call(&call.func) {
            self.reaches = true;
        }
    }
}

impl<'a> Visitor<'a> for ReachesSelf<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Call(call) => {
                self.note_call(call);
            }
            Expr::Attribute(a) if matches!(&*a.value, Expr::Name(n) if n.id.as_str() == "self") => {
                self.self_name = true;
                return;
            }
            Expr::Name(n) if n.id.as_str() == "self" => self.reaches = true,
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}

const HARMLESS_MODULES: &[&str] = &[
    "np", "numpy", "math", "logging", "logger", "log", "warnings",
];

// String literals of the call outside `.alias(..)`: names it may read.
pub(super) struct Mentions(pub(super) Vec<String>);

impl<'a> Visitor<'a> for Mentions {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Call(c) if matches!(&*c.func, Expr::Attribute(a) if a.attr.as_str() == "alias") =>
            {
                self.visit_expr(&c.func);
                return;
            }
            Expr::StringLiteral(lit) => self.0.push(lit.value.to_str().to_string()),
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}

pub(super) struct NoteOpenFrameWriteReads<'c> {
    pub(super) frame: &'c str,
    pub(super) column: &'c str,
    pub(super) found: bool,
}

impl<'a> Visitor<'a> for NoteOpenFrameWriteReads<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Subscript(sub) = expr {
            if matches!(&*sub.value, Expr::Name(n) if n.id.as_str() == self.frame)
                && ast_extract::extract_string_literal(&sub.slice) == Some(self.column)
            {
                self.found = true;
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

pub(super) struct Handed<'f> {
    pub(super) frames: &'f HashMap<String, OpenFrame>,
    pub(super) file_methods: &'f std::collections::HashSet<String>,
    pub(super) handed: Vec<String>,
}

impl<'a> Visitor<'a> for Handed<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call(call) = expr {
            self.note_helper_call(call);
        }
        ast_visitor::walk_expr(self, expr);
    }
}

impl<'f> Handed<'f> {
    fn note_helper_call(&mut self, call: &ruff_python_ast::ExprCall) {
        let reaches_a_helper = match &*call.func {
            Expr::Name(n) => !HARMLESS_CALLEES.contains(&n.id.as_str()),
            Expr::Attribute(a) => {
                self.file_methods.contains(a.attr.as_str())
                    || matches!(&*a.value, Expr::Name(n) if n.id.as_str() == "self")
            }
            _ => true,
        };
        if reaches_a_helper {
            self.passes_tracked_name(call);
        }
    }

    fn passes_tracked_name(&mut self, call: &ruff_python_ast::ExprCall) {
        for arg in call
            .arguments
            .args
            .iter()
            .chain(call.arguments.keywords.iter().map(|k| &k.value))
        {
            if let Expr::Name(n) = arg {
                if self.frames.contains_key(n.id.as_str()) {
                    self.handed.push(n.id.to_string());
                }
            }
        }
    }
}

pub(super) struct Reads<'p> {
    pub(super) param: &'p str,
    pub(super) found: Vec<(String, ruff_text_size::TextSize)>,
}

impl<'a> Visitor<'a> for Reads<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            // A nested lambda or comprehension may rebind the name.
            Expr::Lambda(_)
            | Expr::ListComp(_)
            | Expr::SetComp(_)
            | Expr::DictComp(_)
            | Expr::Generator(_) => return,
            Expr::Subscript(sub) => {
                if matches!(&*sub.value, Expr::Name(n) if n.id.as_str() == self.param) {
                    if let Some(name) = ast_extract::extract_string_literal(&sub.slice) {
                        self.found.push((name.to_string(), sub.range().start()));
                    }
                }
            }
            _ => {}
        }
        ast_visitor::walk_expr(self, expr);
    }
}
