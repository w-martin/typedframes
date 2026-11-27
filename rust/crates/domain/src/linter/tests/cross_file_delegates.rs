use super::*;

#[test]
fn test_should_not_treat_a_decorated_helper_as_a_passthrough() {
    // arrange / act: `@lru_cache` can hand back a shared/stale object; the checker
    // has no way to know that a `df["z"] = 1` inside it is safe to trust.
    let (_, errors) = lint_source(
            "import pandas as pd\nfrom functools import lru_cache\n\n\n@lru_cache\ndef add(df):\n    df[\"z\"] = 1\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = add(df)\nprint(df[\"z\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_still_resolve_a_staticmethod_or_classmethod_helper() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    @staticmethod\n    def add(df):\n        df[\"z\"] = 1\n        return df\n\n    @classmethod\n    def run(cls):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = cls.add(df)\n        print(df[\"z\"])\n        print(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_not_treat_a_decorated_helper_call_as_in_place_when_called_as_a_statement() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef audited(f):\n    return f\n\n\n@audited\ndef add(df):\n    df[\"z\"] = 1\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nadd(df)\nprint(df[\"z\"])\n",
        );

    // assert: `audited` may hand back a wrapper with entirely different behaviour, so
    // the call is not known to mutate `df` at all -- old columns still validated.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_follow_a_delegate_reached_through_a_plainly_imported_module() {
    // arrange / act
    let (_, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nimport helpers\n\n\ndef normalize(df):\n    helpers.fix_in_place(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nnormalize(df)\nprint(df[\"y\"])\nprint(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_not_treat_a_method_call_on_the_parameter_as_a_module_delegate() {
    // arrange / act: `df.merge(df)` -- `df` is both receiver and argument, not a
    // delegate to a module named `df`.
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef combine(df):\n    df.merge(df)\n    return df\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = combine(df)\nprint(df[\"a\"])\n",
        );

    // assert: still recognized as a plain pass-through.
    assert_eq!(linter.dataframes_total, 1);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn test_should_resolve_a_module_qualified_delegate_across_files() {
    // arrange: `middle.py`'s own helper reaches `base.py` through a plain `import base`
    // -- not `from base import add` -- which `resolve_delegate_target` alone can't
    // follow (it only matches `from X import name`/attribute-call-via-any-import).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("base.py"),
        "def add(df):\n    df[\"z\"] = 1\n    return df\n",
    )
    .unwrap();
    fs::write(
        root.join("middle.py"),
        "import base\n\n\ndef normalize(df):\n    base.add(df)\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom middle import normalize\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nnormalize(df)\nprint(df[\"z\"])\nprint(df[\"nope\"])\n";
    fs::write(root.join("main.py"), main_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_resolve_a_self_method_inherited_from_a_base_class_in_another_file() {
    // arrange: Job (main.py) extends Base (base.py); Job's own method mutates `df`
    // through an inherited method it never overrides itself.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("base.py"),
        "class Base:\n    def _add(self, df):\n        df[\"z\"] = 1\n        return df\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom base import Base\n\n\nclass Job(Base):\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = self._add(df)\n        print(df[\"z\"])\n        print(df[\"nope\"])\n";
    fs::write(root.join("main.py"), main_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_prefer_an_override_over_a_cross_file_inherited_method() {
    // arrange: Job overrides `_add` itself, so base.py's version must not apply.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("base.py"),
        "class Base:\n    def _add(self, df):\n        df[\"z\"] = 1\n        return df\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom base import Base\n\n\nclass Job(Base):\n    def _add(self, df):\n        return df.dropna()\n\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = self._add(df)\n        print(df[\"z\"])\n";
    fs::write(root.join("main.py"), main_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();

    // assert: Job's own dropna()-only override wins, so `z` was never added -- a
    // known-unknown column against the concrete {a} schema, not merely unverifiable.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("'z'"));
}

#[test]
fn test_should_follow_a_cross_file_base_that_delegates_to_a_module_function() {
    // arrange: base.py's own method delegates to a module-level helper it imports --
    // two independent kinds of resolution composed in one chain.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("edits.py"),
        "def add(df):\n    df[\"z\"] = 1\n    return df\n",
    )
    .unwrap();
    fs::write(
        root.join("base.py"),
        "from edits import add\n\n\nclass Base:\n    def _add(self, df):\n        return add(df)\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom base import Base\n\n\nclass Job(Base):\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = self._add(df)\n        print(df[\"z\"])\n        print(df[\"nope\"])\n";
    fs::write(root.join("main.py"), main_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_not_treat_an_unresolvable_cross_file_base_method_as_a_passthrough() {
    // arrange: `_add` (in base.py) returns the result of something no file defines.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("base.py"),
        "class Base:\n    def _add(self, df):\n        return opaque_transform(df)\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom base import Base\n\n\nclass Job(Base):\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        df = self._add(df)\n        print(df[\"zzz\"])\n";
    fs::write(root.join("main.py"), main_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_loop_on_a_cross_file_inheritance_cycle() {
    // arrange: a.py's A extends b.py's B, which extends a.py's A right back --
    // pathological, but must not hang the indexer.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("a.py"),
        "from b import B\n\n\nclass A(B):\n    def run(self):\n        return self.other()\n",
    )
    .unwrap();
    fs::write(
        root.join("b.py"),
        "from a import A\n\n\nclass B(A):\n    pass\n",
    )
    .unwrap();

    // act: must terminate.
    let index = build_index_internal(root);

    // assert: indexing completed at all (the assertion above already proves it did).
    assert!(index
        .files
        .contains_key(root.join("a.py").to_str().unwrap()));
}

#[test]
fn test_should_apply_an_in_place_edit_when_the_return_value_is_captured_as_something_else() {
    // arrange / act: `clean` mutates `df` in place but returns a status string --
    // an assignment (`msg = clean(df)`) must not lose that mutation just because the
    // captured value isn't the frame itself.
    let (linter, errors) = lint_source(
            "import pandas as pd\n\n\ndef clean(df):\n    df[\"z\"] = 1\n    return \"done\"\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\", \"b\"])\nmsg = clean(df)\nprint(df[\"z\"])\nprint(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(linter.dataframes_total, 2);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_apply_a_self_methods_in_place_edit_when_its_return_value_is_captured() {
    // arrange / act
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\nclass Job:\n    def clean(self, df):\n        df[\"z\"] = 1\n        return \"done\"\n\n    def run(self):\n        df = pd.read_csv(\"a.csv\", usecols=[\"a\"])\n        msg = self.clean(df)\n        print(df[\"z\"])\n        print(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_untrack_a_name_reassigned_from_a_frame_to_a_helpers_non_frame_result() {
    // arrange / act: `df` is reassigned to `clean`'s return value, a plain string --
    // the OLD `df` binding must not survive under the new name.
    let (_, errors) = lint_source(
            "import pandas as pd\n\n\ndef clean(other):\n    other[\"z\"] = 1\n    return \"done\"\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nother = pd.read_csv(\"b.csv\", usecols=[\"a\"])\ndf = clean(other)\nprint(df[\"zzz\"])\n",
        );

    // assert: `df` no longer names a frame at all, so no unknown/unverifiable claim.
    assert!(
        errors
            .iter()
            .all(|e| e.code != CODE_UNKNOWN_COLUMN && e.code != CODE_UNVERIFIABLE_COLUMN),
        "errors: {errors:?}"
    );
}
