use super::*;

/// What a consumer of an expression's value can do with the objects it refers to.
/// Ordered by how much it endangers a name's value.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Use {
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
pub(super) enum ScopeKind {
    Module,
    Function,
    Class,
    Comprehension,
}

pub(super) enum Binding<'a> {
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

pub(super) struct Scope<'a> {
    pub(super) kind: ScopeKind,
    pub(super) parent: Option<usize>,
    pub(super) bindings: FastMap<&'a str, Vec<Binding<'a>>>,
    pub(super) globals: FastSet<&'a str>,
    pub(super) nonlocals: FastSet<&'a str>,
}

pub(super) struct Occurrence<'a> {
    pub(super) offset: TextSize,
    pub(super) scope: usize,
    pub(super) name: &'a str,
    pub(super) use_: Use,
    // The classification holds only while this builtin is not rebound anywhere.
    pub(super) builtin: Option<&'static str>,
}

pub(super) type GroupKey<'a> = (usize, &'a str);

// The fixed attributes of a module's classes: class name, attribute name, and the attribute's group.
pub(super) type ClassExports<'a> = Vec<(String, String, GroupKey<'a>)>;

// Builtins whose arguments they only inspect.
pub(super) const SAFE_BUILTINS: &[&str] = &[
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
pub(super) const INTERIOR_BUILTINS: &[&str] = &[
    "list",
    "tuple",
    "set",
    "frozenset",
    "sorted",
    "reversed",
    "dict",
    "frozendict",
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
pub(super) const SAFE_METHODS: &[&str] = &[
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
pub(super) const INTERIOR_METHODS: &[&str] = &["values", "items", "get", "copy"];

// DataFrame/Series API methods: they read the column lists, mappings and keys they are
// given and neither change nor keep them, which is what makes `df.select(KEEP)` or
// `df.rename(columns=MAPPING)` safe for a constant to be passed to. Deliberately NOT
// here: anything that is also a container mutator or stores its argument (`append`,
// `extend`, `insert`, `update`, `add`, `setdefault`, ...).
pub(super) const FRAME_METHODS: &[&str] = &[
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
    "frozendict",
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

pub(super) fn annotation_is_plain(annotation: &Expr) -> bool {
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

// `Final`, `Final[T]`, `typing.Final`.
pub(super) fn annotation_is_final(annotation: &Expr) -> bool {
    match annotation {
        Expr::Name(n) => n.id.as_str() == "Final",
        Expr::Attribute(a) => a.attr.as_str() == "Final",
        Expr::Subscript(s) => annotation_is_final(&s.value),
        _ => false,
    }
}

pub(super) struct Collector<'a> {
    pub(super) scopes: Vec<Scope<'a>>,
    pub(super) current: usize,
    pub(super) occurrences: Vec<Occurrence<'a>>,
    pub(super) pending_nonlocal: Vec<(usize, &'a str)>,
    // `Cls.NAME` loads, resolved to a class attribute once every scope is known.
    pub(super) attribute_occurrences: Vec<AttributeOccurrence<'a>>,
    // `Cls.NAME = ...` / `del Cls.NAME` / `Cls.NAME += ...`: (scope, `Cls`, `NAME`).
    pub(super) pending_attribute_stores: Vec<(usize, &'a str, &'a str)>,
    // Names bound by `NAME: Final = ...`, which promises no subclass rebinds them.
    pub(super) final_bindings: HashSet<(usize, &'a str)>,
    // `setattr(Cls, ...)` / `delattr(Cls, ...)` / `vars(Cls)`: (scope, `Cls`).
    pub(super) dynamic_class_targets: Vec<(usize, &'a str)>,
    // `globals()`/`vars()`/`exec`/`eval`/`locals()` or a star import: the module
    // namespace can change in ways no binding records.
    pub(super) dynamic_namespace: bool,
    pub(super) builtin: Option<&'static str>,
}

pub(super) struct AttributeOccurrence<'a> {
    pub(super) range: TextRange,
    pub(super) scope: usize,
    pub(super) base: &'a str,
    pub(super) attr: &'a str,
    pub(super) use_: Use,
}
