use super::*;

// Methods that could hand an instance attribute a value other than the class default.
const INSTANCE_HOOKS: &[&str] = &[
    "__init__",
    "__post_init__",
    "__new__",
    "__setattr__",
    "__getattr__",
    "__getattribute__",
];

// The class of `key` when it is bound once to `Cls()`, `Cls` a frozen dataclass of this
// module (as are its bases) that defines no hook able to change an attribute.
pub(super) fn frozen_dataclass_instance_class<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    key: GroupKey<'a>,
) -> Option<GroupKey<'a>> {
    let group = groups.get(&key)?;
    let Some(Expr::Call(call)) = group.assign else {
        return None;
    };
    if group.bindings != 1 || !call.arguments.is_empty() {
        return None;
    }
    let Expr::Name(class) = &*call.func else {
        return None;
    };
    let class_key = lookup(scopes, key.0, class.id.as_str())?;
    let mut current = class_key;
    for _ in 0..8 {
        let (def, body) = groups.get(&current)?.class?;
        if dataclass_decorator(groups, scopes, def) != Some(true)
            || INSTANCE_HOOKS
                .iter()
                .any(|hook| scopes[body].bindings.contains_key(hook))
        {
            return None;
        }
        let parent = match def.arguments.as_deref().map(|a| a.args.as_ref()) {
            None | Some([]) => return Some(class_key),
            Some([Expr::Name(parent)]) => parent,
            Some(_) => return None,
        };
        current = lookup(scopes, scopes[body].parent?, parent.id.as_str())?;
    }
    None
}

// `Some(frozen)` when `def`'s only decorator is `dataclasses.dataclass`, bare or called with
// options that keep fields as class attributes (`slots=True` does not).
pub(super) fn dataclass_decorator<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    def: &ast::StmtClassDef,
) -> Option<bool> {
    let [decorator] = def.decorator_list.as_slice() else {
        return None;
    };
    let (callee, keywords) = match &decorator.expression {
        Expr::Call(call) if call.arguments.args.is_empty() => {
            (&*call.func, call.arguments.keywords.as_ref())
        }
        other => (other, &[][..]),
    };
    let imported = |name: &str| {
        lookup(scopes, 0, name)
            .and_then(|k| groups.get(&k))
            .filter(|g| g.bindings == 1)
            .and_then(|g| g.import.as_ref())
            .cloned()
    };
    let is_dataclass = match callee {
        Expr::Name(n) => matches!(
            imported(n.id.as_str()),
            Some(ImportRef::From(0, m, d)) if m == "dataclasses" && d == "dataclass"
        ),
        Expr::Attribute(a) => match &*a.value {
            Expr::Name(n) => {
                a.attr.as_str() == "dataclass"
                    && matches!(
                        imported(n.id.as_str()),
                        Some(ImportRef::Module(0, m)) if m == "dataclasses"
                    )
            }
            _ => false,
        },
        _ => false,
    };
    if !is_dataclass {
        return None;
    }
    let mut frozen = false;
    for keyword in keywords {
        let flag = match &keyword.value {
            Expr::BooleanLiteral(b) => b.value,
            _ => return None,
        };
        match keyword.arg.as_ref()?.as_str() {
            "frozen" => frozen = flag,
            "init" | "repr" | "eq" | "order" | "unsafe_hash" | "kw_only" | "match_args" => {}
            _ => return None,
        }
    }
    Some(frozen)
}

// The class bodies whose bindings make up a string enum's names: its own, and those of
// the member-less string enums of this module it derives from (`class Col(Base)`).
pub(super) fn string_enum_bodies<'a>(
    groups: &FastMap<GroupKey<'a>, Group<'a>>,
    scopes: &[Scope<'a>],
    mut def: &'a ast::StmtClassDef,
    mut body: usize,
) -> Option<Vec<usize>> {
    let mut bodies = vec![body];
    for _ in 0..8 {
        if is_string_enum(def) {
            return Some(bodies);
        }
        let arguments = def.arguments.as_deref()?;
        let [Expr::Name(base)] = arguments.args.as_ref() else {
            return None;
        };
        if !def.decorator_list.is_empty() || !arguments.keywords.is_empty() {
            return None;
        }
        let group = groups.get(&lookup(scopes, scopes[body].parent?, base.id.as_str())?)?;
        if group.bindings != 1 {
            return None;
        }
        (def, body) = group.class?;
        bodies.push(body);
    }
    None
}

// `class Col(StrEnum)` / `class Col(str, Enum)` with no decorator, metaclass or other base:
// its members are exactly the names its body binds.
fn is_string_enum(def: &ast::StmtClassDef) -> bool {
    let Some(arguments) = def.arguments.as_deref() else {
        return false;
    };
    let bases: Vec<&str> = arguments
        .args
        .iter()
        .map(|base| match base {
            Expr::Name(n) => n.id.as_str(),
            _ => "",
        })
        .collect();
    def.decorator_list.is_empty()
        && arguments.keywords.is_empty()
        && bases
            .iter()
            .all(|b| matches!(*b, "StrEnum" | "Enum" | "str"))
        && (bases.contains(&"StrEnum") || (bases.contains(&"str") && bases.contains(&"Enum")))
}

/// `Col.NOPE` where `Col` is a string enum (this file's, or imported with
/// `from module import Col`) whose body binds no `NOPE`: an AttributeError when it runs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MissingMember {
    pub(crate) range: TextRange,
    pub(crate) class: String,
    pub(crate) attr: String,
    pub(crate) members: Vec<String>,
}

impl Analysis<'_> {
    pub(crate) fn missing_enum_members(&self, module: &ModModule) -> Vec<MissingMember> {
        if !self.complete || self.dynamic_namespace {
            return Vec::new();
        }
        let mut finder = Finder {
            analysis: self,
            found: Vec::new(),
        };
        for stmt in &module.body {
            finder.visit_stmt(stmt);
        }
        finder.found
    }

    fn enum_members_of(&self, name: &ast::ExprName) -> Option<Vec<String>> {
        let key = self.occurrence_group.get(&name.range().start())?.as_ref()?;
        let group = self.groups.get(key)?;
        if key.0 != 0 || group.bindings != 1 {
            return None;
        }
        if group.class.is_some() {
            return self.string_enums.get(key.1).cloned();
        }
        let Some(ImportRef::From(level, module, imported)) = &group.import else {
            return None;
        };
        self.imports
            .get(&(*level, module.clone()))?
            .enums
            .get(imported)
            .cloned()
    }
}

struct Finder<'s, 'a> {
    pub(super) analysis: &'s Analysis<'a>,
    pub(super) found: Vec<MissingMember>,
}

impl<'v> Visitor<'v> for Finder<'_, '_> {
    fn visit_expr(&mut self, expr: &'v Expr) {
        if let Expr::Attribute(a) = expr {
            let attr = a.attr.as_str();
            // Members are spelled in capitals; a lower-case name may be a method
            // the enum inherits from `str`.
            let member_like = a.ctx == ExprContext::Load
                && attr.starts_with(|c: char| c.is_ascii_uppercase())
                && attr
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            let class = match &*a.value {
                Expr::Name(class) if member_like => self
                    .analysis
                    .enum_members_of(class)
                    .map(|m| (class.id.to_string(), m)),
                // `cols.Col.NOPE` through `import pkg.cols as cols`.
                Expr::Attribute(inner) if member_like => self
                    .analysis
                    .imported_member(inner)
                    .and_then(|m| m.enums.get(inner.attr.as_str()))
                    .map(|m| (inner.attr.to_string(), m.clone())),
                _ => None,
            };
            if let Some((class, members)) = class {
                if !members.iter().any(|m| m == attr) {
                    self.found.push(MissingMember {
                        range: a.range(),
                        class,
                        attr: attr.to_string(),
                        members,
                    });
                }
            }
        }
        ast_visitor::walk_expr(self, expr);
    }
}

// `from module import name`: a submodule when the importer found one by that name and the
// package itself defines nothing called `name`.
pub(super) fn import_from_ref(
    imports: &ImportedModules,
    level: u32,
    module: &str,
    name: &str,
) -> ImportRef {
    let submodule = if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}.{name}")
    };
    let defined_by_package = imports.get(&(level, module.to_string())).is_some_and(|m| {
        let prefix = format!("{name}.");
        m.constants.contains_key(name)
            || m.functions.contains_key(name)
            || m.enums.contains_key(name)
            || m.constants.keys().any(|k| k.starts_with(&prefix))
    });
    if !defined_by_package && imports.contains_key(&(level, submodule.clone())) {
        ImportRef::Module(level, submodule)
    } else {
        ImportRef::From(level, module.to_string(), name.to_string())
    }
}

// Bases whose subclasses are enumerations: their members are the class's own assignments.
pub(super) const ENUM_BASES: &[&str] = &["Enum", "StrEnum", "IntEnum", "str", "int"];
