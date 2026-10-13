//! What a bug-report snippet may keep verbatim. Everything else in the source is renamed
//! (see `report`), so this file is the whole list of names that can survive redaction:
//! read it to know what a snippet can reveal.

use std::collections::HashSet;
use std::sync::OnceLock;

/// Python builtins, and the dunder/self names every class uses.
const PYTHON: &[&str] = &[
    "self",
    "cls",
    "print",
    "len",
    "list",
    "dict",
    "set",
    "tuple",
    "str",
    "int",
    "float",
    "bool",
    "bytes",
    "range",
    "sorted",
    "reversed",
    "sum",
    "min",
    "max",
    "any",
    "all",
    "zip",
    "enumerate",
    "map",
    "filter",
    "isinstance",
    "issubclass",
    "type",
    "open",
    "abs",
    "round",
    "next",
    "iter",
    "getattr",
    "setattr",
    "hasattr",
    "super",
    "object",
    "property",
    "staticmethod",
    "classmethod",
    "Exception",
    "ValueError",
    "KeyError",
    "TypeError",
    "RuntimeError",
    "NotImplementedError",
    "args",
    "kwargs",
    "__init__",
    "__name__",
    "__main__",
    "__call__",
    "__enter__",
    "__exit__",
    "__post_init__",
    "match",
    "case",
    "type",
    "_",
    "Annotated",
    "Optional",
    "Union",
    "Any",
    "List",
    "Dict",
    "Tuple",
    "Callable",
    "TYPE_CHECKING",
    "Literal",
    "Sequence",
    "Iterable",
    "Mapping",
    "cast",
    "BaseSchema",
    "Column",
    "ColumnSet",
    "ColumnGroup",
    "annotations",
    "__repr__",
    "__str__",
    "__len__",
    "__getitem__",
    "__iter__",
    "__eq__",
    "__hash__",
    "__new__",
    "typing",
    "typedframes",
    "connectorx",
    "__future__",
];

/// Library names the generated list (`library_names.txt`) lacks: aliases, names from other
/// versions, and the methods of other libraries that return a frame.
const LIBRARY: &[&str] = &[
    "pandas",
    "polars",
    "numpy",
    "pd",
    "pl",
    "np",
    "NaN",
    "pl_col",
    "then",
    "otherwise",
    "applymap",
    "with_column",
    "to_polars",
    "to_df",
    "to_dataframe",
    "fetch_pandas_all",
    "toPandas",
    "setdefault",
    "category",
];

/// Short string values that are options rather than data (`how="left"`, `axis="columns"`).
const OPTION_STRINGS: &[&str] = &[
    "left",
    "right",
    "inner",
    "outer",
    "cross",
    "anti",
    "semi",
    "full",
    "columns",
    "index",
    "rows",
    "all",
    "any",
    "first",
    "last",
    "none",
    "sum",
    "mean",
    "median",
    "min",
    "max",
    "count",
    "std",
    "var",
    "size",
    "nunique",
    "ascending",
    "descending",
    "pyarrow",
    "c",
    "python",
    "utf-8",
    "polars",
    "pandas",
    "ignore",
    "raise",
    "coerce",
    "linear",
    "ffill",
    "bfill",
];

/// Keywords whose string value is an option (`how="left"`), not data. A string anywhere
/// else is renamed, even if it spells one of `OPTION_STRINGS`.
const OPTION_KEYWORDS: &[&str] = &[
    "how",
    "axis",
    "engine",
    "keep",
    "method",
    "join",
    "orient",
    "errors",
    "mode",
    "na_position",
    "closed",
    "side",
    "kind",
    "validate",
    "indicator",
];

/// Import roots a snippet keeps; any other import is dropped.
pub(crate) const KEPT_IMPORT_ROOTS: &[&str] = &[
    "pandas",
    "polars",
    "numpy",
    "typing",
    "typedframes",
    "connectorx",
    "__future__",
];

/// Names generated from the installed numpy, pandas and polars (see
/// `inv names`), by where in the code a name stands.
struct Generated {
    module: HashSet<&'static str>,
    member: HashSet<&'static str>,
    keyword: HashSet<&'static str>,
}

fn generated() -> &'static Generated {
    static GENERATED: OnceLock<Generated> = OnceLock::new();
    GENERATED.get_or_init(|| {
        let mut names = Generated {
            module: HashSet::new(),
            member: HashSet::new(),
            keyword: HashSet::new(),
        };
        for line in include_str!("library_names.txt").lines() {
            let Some((tag, name)) = line.split_at_checked(1) else {
                continue;
            };
            match tag {
                "m" => names.module.insert(name),
                "a" => names.member.insert(name),
                "k" => names.keyword.insert(name),
                _ => continue,
            };
        }
        names
    })
}

fn kept() -> &'static HashSet<&'static str> {
    static KEPT: OnceLock<HashSet<&'static str>> = OnceLock::new();
    KEPT.get_or_init(|| {
        let mut set: HashSet<&'static str> = HashSet::new();
        set.extend(PYTHON);
        set.extend(LIBRARY);
        set.extend(crate::constants::RESERVED_METHODS);
        set.extend(crate::constants::LOAD_FUNCTIONS);
        set.extend(crate::constants::SCHEMA_PRESERVING_METHODS);
        set.extend(crate::constants::NON_FRAME_METHODS);
        set.extend(crate::constants::ROW_PASSTHROUGH_METHODS);
        set.extend(crate::constants::SQL_LOAD_FUNCTIONS);
        set.extend(crate::column_consuming_methods::names());
        set
    })
}

/// Whether a source name may stay as written.
pub(crate) fn keeps_name(name: &str) -> bool {
    kept().contains(name)
}

/// Whether a library name may stay as written where it stands, by the renaming prefix of
/// its position: `f` a called name, `a` an attribute, `k` a call keyword, anything else a
/// bare name. A member name like `value` stays only after a dot or as a call, and a
/// parameter name only as a keyword, so an ordinary variable of that name is still renamed.
pub(crate) fn keeps_library_name(name: &str, prefix: char) -> bool {
    let names = generated();
    match prefix {
        'k' => names.keyword.contains(name),
        'a' | 'f' => names.module.contains(name) || names.member.contains(name),
        _ => names.module.contains(name),
    }
}

/// Whether a string literal's contents may stay as written, given the keyword it is the
/// value of, if any.
pub(crate) fn keeps_string(contents: &str, keyword: Option<&str>) -> bool {
    keyword.is_some_and(|k| OPTION_KEYWORDS.contains(&k)) && OPTION_STRINGS.contains(&contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_keep_library_and_python_names_but_not_ordinary_words() {
        assert!(keeps_name("self"));
        assert!(keeps_library_name("read_csv", 'f'));
        assert!(keeps_library_name("merge", 'f'));
        assert!(keeps_library_name("usecols", 'k'));
        assert!(!keeps_name("revenue"));
        assert!(!keeps_name("customer_id"));
        assert!(!keeps_name("PricingService"));
        assert!(!keeps_name("result"));
        assert!(!keeps_name("value"));
    }

    #[test]
    fn test_should_keep_a_library_name_only_where_it_is_used_as_that_kind_of_name() {
        assert!(keeps_library_name("percentile", 'f'));
        assert!(keeps_library_name("ndarray", 'v'));
        assert!(keeps_library_name("groupby", 'a'));
        assert!(!keeps_library_name("groupby", 'v'));
        assert!(keeps_library_name("numeric_only", 'k'));
        assert!(!keeps_library_name("numeric_only", 'v'));
        assert!(!keeps_library_name("revenue", 'v'));
        assert!(!keeps_library_name("revenue", 'k'));
    }

    #[test]
    fn test_should_keep_option_strings_but_not_data() {
        assert!(keeps_string("left", Some("how")));
        assert!(!keeps_string("left", Some("name")));
        assert!(!keeps_string("left", None));
        assert!(!keeps_string("count", None));
        assert!(!keeps_string("loss", Some("how")));
    }
}
