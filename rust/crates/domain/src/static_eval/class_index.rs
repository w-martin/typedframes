use super::*;

// `class_attribute` for the class whose name group is `key`.
pub(super) fn class_attribute_of<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    dynamic: &FastSet<usize>,
    mut key: GroupKey<'a>,
    attr: &'a str,
) -> Option<GroupKey<'a>> {
    let mut chain: Vec<usize> = Vec::new();
    loop {
        let group = groups.get(&key)?;
        let (def, body) = group.class?;
        if group.bindings != 1
            || dynamic.contains(&body)
            || (!def.decorator_list.is_empty()
                && dataclass_decorator(groups, scopes, def).is_none())
            || chain.len() >= 8
            || chain.contains(&body)
        {
            return None;
        }
        chain.push(body);
        let Some(arguments) = def.arguments.as_deref() else {
            break;
        };
        if !arguments.keywords.is_empty() {
            return None;
        }
        // `class Col(StrEnum)` / `class Col(str, Enum)`: the members are plain class attributes.
        let mut parents = arguments
            .args
            .iter()
            .filter(|base| !matches!(base, Expr::Name(n) if ENUM_BASES.contains(&n.id.as_str())));
        let parent = parents.next();
        if parents.next().is_some() {
            return None;
        }
        match parent {
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
        .find(|body| groups.contains_key(&(**body, attr)))
        .map(|body| (*body, attr))
}

// What `self`/`cls` lookups need to know about a file's classes, built once: a class body's
// name group, and which classes list a given name as a base.
pub(super) struct ClassIndex<'a> {
    pub(super) by_body: FastMap<usize, GroupKey<'a>>,
    pub(super) children: FastMap<&'a str, Vec<(&'a str, usize)>>,
}

impl<'a> ClassIndex<'a> {
    pub(super) fn new(groups: &FastMap<GroupKey<'a>, Group<'a>>) -> Self {
        let mut by_body = FastMap::default();
        let mut children: FastMap<&'a str, Vec<(&'a str, usize)>> = FastMap::default();
        for (key, group) in groups {
            let Some((def, body)) = group.class else {
                continue;
            };
            by_body.insert(body, *key);
            for base in def.arguments.iter().flat_map(|a| a.args.iter()) {
                if let Expr::Name(base) = base {
                    children
                        .entry(base.id.as_str())
                        .or_default()
                        .push((key.1, body));
                }
            }
        }
        ClassIndex { by_body, children }
    }
}

// The group of the class whose method `from` is in, when `base` is that method's own `self`
// or `cls` (bound once, so not reassigned).
pub(super) fn enclosing_class<'a>(
    classes: &ClassIndex<'a>,
    scopes: &[Scope<'a>],
    from: usize,
    base: &str,
) -> Option<GroupKey<'a>> {
    if !matches!(base, "self" | "cls") {
        return None;
    }
    let mut method = from;
    while matches!(scopes[method].kind, ScopeKind::Comprehension) {
        method = scopes[method].parent?;
    }
    if !matches!(scopes[method].kind, ScopeKind::Function)
        || scopes[method].bindings.get(base).map(Vec::len) != Some(1)
    {
        return None;
    }
    let body = scopes[method].parent?;
    if !matches!(scopes[body].kind, ScopeKind::Class) {
        return None;
    }
    classes.by_body.get(&body).cloned()
}

// Whether a class derived (in this file) from `class_name`, directly or not, binds `attr`:
// then what `self.attr` is depends on which class `self` is an instance of.
fn rebound_in_subclass(
    classes: &ClassIndex,
    scopes: &[Scope],
    class_name: &str,
    attr: &str,
) -> bool {
    let mut parents = vec![class_name];
    let mut next = 0;
    while next < parents.len() {
        let parent = parents[next];
        next += 1;
        for (name, body) in classes.children.get(parent).into_iter().flatten() {
            if scopes[*body].bindings.contains_key(attr) {
                return true;
            }
            if !parents.contains(name) {
                parents.push(name);
            }
        }
    }
    false
}

// The group `base.attr` reads or writes: a class attribute reached by the class's name, or
// through `self`/`cls` in one of its methods when no class derived from it rebinds `attr`
// (a subclass in another file can't be seen).
pub(super) fn attribute_group_of<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    classes: &ClassIndex<'a>,
    scopes: &[Scope<'a>],
    dynamic: &FastSet<usize>,
    from: usize,
    base: &str,
    attr: &'a str,
) -> Option<GroupKey<'a>> {
    match enclosing_class(classes, scopes, from, base) {
        Some(class) => {
            if rebound_in_subclass(classes, scopes, class.1, attr) {
                return None;
            }
            let key = class_attribute_of(groups, scopes, dynamic, class, attr)?;
            // An empty placeholder is what a subclass elsewhere is expected to fill in.
            let placeholder =
                groups
                    .get(&key)
                    .and_then(|g| g.assign)
                    .is_some_and(|rhs| match rhs {
                        Expr::List(l) => l.elts.is_empty(),
                        Expr::Tuple(t) => t.elts.is_empty(),
                        Expr::Set(s) => s.elts.is_empty(),
                        Expr::Dict(d) => d.items.is_empty(),
                        _ => false,
                    });
            (!placeholder).then_some(key)
        }
        None => class_attribute(groups, scopes, dynamic, from, base, attr),
    }
}

// Python's name lookup from `from` outward: the scope's own binding, else the nearest
// enclosing one, not counting class scopes (a method can't see its class's names).
pub(super) fn lookup<'a>(scopes: &[Scope<'a>], from: usize, name: &str) -> Option<GroupKey<'a>> {
    let scope = &scopes[from];
    if let Some(declared) = scope.globals.get(name) {
        return Some((0, *declared));
    }
    if scope.nonlocals.contains(name) {
        let mut parent = scope.parent;
        while let Some(i) = parent {
            if scopes[i].kind == ScopeKind::Function {
                if let Some((bound, _)) = scopes[i].bindings.get_key_value(name) {
                    return Some((i, *bound));
                }
            }
            parent = scopes[i].parent;
        }
        return None;
    }
    if let Some((bound, _)) = scope.bindings.get_key_value(name) {
        return Some((from, *bound));
    }
    let mut parent = scope.parent;
    while let Some(i) = parent {
        if scopes[i].kind != ScopeKind::Class {
            if let Some((bound, _)) = scopes[i].bindings.get_key_value(name) {
                return Some((i, *bound));
            }
        }
        parent = scopes[i].parent;
    }
    None
}

// Already a literal: nothing to fold.
pub(super) fn is_plain_literal(expr: &Expr) -> bool {
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
