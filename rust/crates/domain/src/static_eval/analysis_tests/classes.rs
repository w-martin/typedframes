use super::*;

#[test]
fn test_should_read_a_class_attribute_by_class_name() {
    let source = "class Cols:\n    NAMES = [\"a\", \"b\"]\n\n\nprint(usecols=Cols.NAMES)\n";
    assert_eq!(folded(source, "Cols.NAMES"), Some(strs(&["a", "b"])));
}

#[test]
fn test_should_read_an_annotated_class_attribute_and_one_built_from_another() {
    let source = "from typing import ClassVar\n\n\nclass C:\n    A: ClassVar[list[str]] = [\"a\"]\n    B = A + [\"b\"]\n\n\nprint(usecols=C.B)\n";
    assert_eq!(folded(source, "C.B"), Some(strs(&["a", "b"])));
}

#[test]
fn test_should_inherit_a_class_attribute_from_a_single_user_defined_base() {
    let source = "class Base:\n    X = [\"a\"]\n    Y = [\"base\"]\n\n\nclass Child(Base):\n    Y = [\"child\"]\n\n\nprint(a=Child.X, b=Child.Y)\n";
    assert_eq!(folded(source, "Child.X"), Some(strs(&["a"])));
    assert_eq!(folded(source, "Child.Y"), Some(strs(&["child"])));
}

#[test]
fn test_should_refuse_a_class_it_cannot_vouch_for() {
    for header in [
        "class Cols(BaseSchema):",
        "class Cols(Base1, Base2):",
        "class Cols(metaclass=Meta):",
        "class Cols(Imported):",
        "@dataclass\nclass Cols:",
    ] {
        let source = format!("{header}\n    NAMES = [\"a\"]\n\n\nprint(usecols=Cols.NAMES)\n");
        assert_eq!(folded(&source, "Cols.NAMES"), None, "{header}");
    }
}

#[test]
fn test_should_resolve_the_fields_of_a_frozen_dataclass_instance_built_without_arguments() {
    let dataclass = "from dataclasses import dataclass, field\nimport dataclasses\n\n\n";
    let class = |decorator: &str, extra: &str| {
        format!("{dataclass}{decorator}\nclass N:\n    a: str = \"a\"\n{extra}\n\n")
    };
    let resolved = [
        format!(
            "{}X = N()\nprint(X.a)\n",
            class("@dataclass(frozen=True)", "")
        ),
        format!(
            "{}X = N()\nprint(X.a)\n",
            class("@dataclasses.dataclass(frozen=True)", "")
        ),
        format!("{}print(N.a)\n", class("@dataclass", "")),
    ];
    for source in &resolved {
        let expr = if source.contains("X.a") { "X.a" } else { "N.a" };
        assert_eq!(
            folded(source, expr),
            Some(Value::Str("a".into())),
            "{source}"
        );
    }
    let post_init =
        "    def __post_init__(self):\n        object.__setattr__(self, \"a\", \"b\")\n";
    let refused = [
        // can be changed after it is built
        format!("{}X = N()\nprint(X.a)\n", class("@dataclass", "")),
        format!(
            "{}X = N()\nprint(X.a)\n",
            class("@dataclass(frozen=True)", post_init)
        ),
        // built with a value, or rebound
        format!(
            "{}X = N(a=\"b\")\nprint(X.a)\n",
            class("@dataclass(frozen=True)", "")
        ),
        format!(
            "{}X = N()\nX = N()\nprint(X.a)\n",
            class("@dataclass(frozen=True)", "")
        ),
        // a slotted class holds no defaults
        format!(
            "{}X = N()\nprint(X.a)\n",
            class("@dataclass(frozen=True, slots=True)", "")
        ),
        format!("{}print(N.a)\n", class("@dataclass(slots=True)", "")),
    ];
    for source in &refused {
        assert_eq!(
            folded(source, "X.a").or(folded(source, "N.a")),
            None,
            "{source}"
        );
    }
}

#[test]
fn test_should_list_the_members_of_an_enum_derived_from_a_member_less_one() {
    let source = "from enum import StrEnum\n\n\nclass Base(StrEnum):\n    pass\n\n\nclass Col(Base):\n    A = \"a\"\n\n\nprint(Col.A, Col.B)\n";
    let parsed = ruff_python_parser::parse_module(source).unwrap();
    let imports = ImportedModules::new();
    let analysis = Analysis::build(parsed.syntax(), &imports);

    let missing = analysis.missing_enum_members(parsed.syntax());

    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!(
        (missing[0].class.as_str(), missing[0].attr.as_str()),
        ("Col", "B")
    );
}

#[test]
fn test_should_resolve_the_members_of_an_enumeration() {
    for header in [
        "class Cols(StrEnum):",
        "class Cols(Enum):",
        "class Cols(str, Enum):",
    ] {
        let source = format!("{header}\n    HR = \"hr\"\n\n\nprint(usecols=Cols.HR)\n");
        assert_eq!(
            folded(&source, "Cols.HR"),
            Some(Value::Str("hr".into())),
            "{header}"
        );
    }
}

#[test]
fn test_should_resolve_the_value_of_an_enumeration_member() {
    let source = "class Cols(StrEnum):\n    HR = \"hr\"\n\n\nprint(usecols=Cols.HR.value)\n";
    assert_eq!(
        folded(source, "Cols.HR.value"),
        Some(Value::Str("hr".into()))
    );
}

#[test]
fn test_should_refuse_a_class_attribute_that_can_change() {
    for change in [
        "Cols.NAMES = [\"z\"]",
        "Cols.NAMES += [\"z\"]",
        "del Cols.NAMES",
        "Cols.NAMES.append(\"z\")",
        "Cols.NAMES[0] = \"z\"",
        "setattr(Cols, \"NAMES\", [])",
        "delattr(Cols, \"NAMES\")",
        "vars(Cols)",
        "alias = Cols.NAMES",
        "helper(Cols.NAMES)",
    ] {
        let source =
            format!("class Cols:\n    NAMES = [\"a\"]\n\n\n{change}\nprint(usecols=Cols.NAMES)\n");
        assert_eq!(folded(&source, "Cols.NAMES"), None, "{change}");
    }
}

#[test]
fn test_should_refuse_an_attribute_the_class_does_not_define_once() {
    let twice =
        "class Cols:\n    NAMES = [\"a\"]\n    NAMES = [\"b\"]\n\n\nprint(usecols=Cols.NAMES)\n";
    assert_eq!(folded(twice, "Cols.NAMES"), None);
    let missing = "class Cols:\n    NAMES = [\"a\"]\n\n\nprint(usecols=Cols.OTHER)\n";
    assert_eq!(folded(missing, "Cols.OTHER"), None);
    let rebound = "class Cols:\n    NAMES = [\"a\"]\n\n\nCols = Other\nprint(usecols=Cols.NAMES)\n";
    assert_eq!(folded(rebound, "Cols.NAMES"), None);
}

#[test]
fn test_should_not_read_a_class_attribute_through_self_or_an_instance_or_nesting() {
    let source = "class Cols:\n    NAMES = [\"a\"]\n\n    def m(self):\n        return self.NAMES\n\n\nc = Cols()\nprint(a=c.NAMES)\n";
    assert_eq!(folded(source, "c.NAMES"), None);
    let nested =
        "class Outer:\n    class Inner:\n        X = [\"a\"]\n\n\nprint(a=Outer.Inner.X)\n";
    assert_eq!(folded(nested, "Outer.Inner.X"), None);
}

#[test]
fn test_should_not_call_a_method_through_its_class() {
    let source = "class C:\n    def m(self):\n        return [\"a\"]\n\n\nprint(a=C.m())\n";
    assert_eq!(folded(source, "C.m()"), None);
}

#[test]
fn test_should_leave_a_name_that_is_also_a_class_attribute_independent() {
    // `NAMES` at module level and `Cols.NAMES` are different variables
    let source = "NAMES = [\"module\"]\n\n\nclass Cols:\n    NAMES = [\"class\"]\n\n\nprint(a=NAMES, b=Cols.NAMES)\n";
    assert_eq!(folded(source, "NAMES"), Some(strs(&["module"])));
    assert_eq!(folded(source, "Cols.NAMES"), Some(strs(&["class"])));
}
