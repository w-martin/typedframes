use super::*;

const MAX_FOLDED_NODES: usize = 1_000;

pub(super) fn node_count(value: &Value) -> usize {
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
    pub(super) evaluator: &'e Evaluator<'e>,
    pub(super) sites: HashMap<TextRange, Value>,
    // Bare names being accessed, indexed or called. Replacing one gains nothing (the
    // whole expression was tried first) and risks hiding a variable the checker tracks.
    pub(super) protected: HashSet<TextRange>,
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
    pub(super) sites: &'s HashMap<TextRange, Value>,
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
pub(super) fn find_sites(
    module: &ModModule,
    analysis: &Analysis,
) -> (HashMap<TextRange, Value>, Vec<SelfConstantRead>) {
    if !analysis.complete {
        return (HashMap::new(), Vec::new());
    }
    let evaluator = Evaluator::new(analysis);
    let mut finder = SiteFinder {
        evaluator: &evaluator,
        sites: HashMap::new(),
        protected: HashSet::new(),
    };
    for stmt in &module.body {
        finder.visit_stmt(stmt);
    }
    // Only a read that ended up inside a folded value changed what the checker sees.
    let reads = analysis
        .self_reads
        .iter()
        .filter(|read| {
            finder
                .sites
                .iter()
                .any(|(site, value)| site.contains_range(read.range) && value.can_name_columns())
        })
        .cloned()
        .collect();
    (finder.sites, reads)
}

/// The expressions of a module that evaluate statically, and what they evaluate to.
pub(crate) struct Folds(HashMap<TextRange, Value>);

/// What `apply_folds` would replace in `module`, and the class constants read through
/// `self`/`cls` that are among it. Empty when the analysis can't be trusted for the file.
pub(crate) fn plan_folds(
    module: &ModModule,
    analysis: &Analysis,
) -> (Folds, Vec<SelfConstantRead>) {
    let (sites, reads) = find_sites(module, analysis);
    (Folds(sites), reads)
}

/// Replaces every planned expression with the literal it equals, keeping source ranges so
/// diagnostics still point at the original text. In place: the planning step is what needed
/// the module as written, so a caller that is done with that owns nothing it must copy.
pub(crate) fn apply_folds(module: &mut ModModule, folds: &Folds) {
    if folds.0.is_empty() {
        return;
    }
    let folder = Folder { sites: &folds.0 };
    for stmt in &mut module.body {
        folder.visit_stmt(stmt);
    }
}

#[cfg(test)]
pub(crate) fn fold_constants(module: &ModModule, imports: &ImportedModules) -> Option<ModModule> {
    let (folds, _) = plan_folds(module, &Analysis::build(module, imports));
    if folds.0.is_empty() {
        return None;
    }
    let mut folded = module.clone();
    apply_folds(&mut folded, &folds);
    Some(folded)
}

#[cfg(test)]
pub(super) struct NoNames;

#[cfg(test)]
impl NameResolver for NoNames {
    fn resolve(&self, _: &ast::ExprName) -> Resolved {
        Resolved::Unknown
    }
}
