use super::*;

#[test]
fn test_should_keep_same_named_inferred_schemas_of_different_files_apart() {
    // Both loaders make `df` on line 5, so both infer a schema with the same local
    // name; which columns main sees must not depend on map iteration order.
    let loader = |cols: &str| {
        format!("import pandas as pd\n\n\ndef load(path):\n    df = pd.read_csv(path, usecols=[{cols}])\n    return df\n")
    };
    let main = "from loaders import load\n\ndf = load(\"f.csv\")\nprint(df[\"zz\"])\n";
    for _ in 0..40 {
        let (_, errors) = check_in_project(
            &[
                ("loaders.py", &loader("\"a\", \"b\"")),
                ("other.py", &loader("\"x\", \"y\"")),
                ("another.py", &loader("\"p\", \"q\"")),
            ],
            main,
        );
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(
            errors[0].message.contains("{a, b}"),
            "{}",
            errors[0].message
        );
    }
}

#[test]
fn test_should_resolve_a_constant_imported_from_another_file() {
    let main = "import pandas as pd\nfrom config import METRICS\n\ndf = pd.read_csv(\"f.csv\", usecols=METRICS)\nprint(df[\"m1\"])\nprint(df[\"zz\"])\n";
    let (linter, errors) = check_in_project(&[("config.py", CONFIG)], main);

    assert_eq!(linter.dataframes_typed, 1);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("{id, amount, m0, m1}"));
}

#[test]
fn test_should_resolve_a_constant_through_a_module_import() {
    let main = "import pandas as pd\nimport config as cfg\n\ndf = pd.read_csv(\"f.csv\", usecols=cfg.BASE)\nprint(df[\"zz\"])\n";
    let (linter, errors) = check_in_project(&[("config.py", CONFIG)], main);

    assert_eq!(linter.dataframes_typed, 1);
    assert!(errors[0].message.contains("{id, amount}"), "{errors:?}");
}

#[test]
fn test_should_call_a_function_imported_from_another_file() {
    let main = "import pandas as pd\nimport config\nfrom config import cols\n\na = pd.read_csv(\"f.csv\", usecols=cols(\"x\", n=2))\nb = pd.read_csv(\"g.csv\", usecols=config.cols(\"y\"))\nprint(a[\"zz\"])\nprint(b[\"zz\"])\n";
    let (linter, errors) = check_in_project(&[("config.py", CONFIG)], main);

    assert_eq!(linter.dataframes_typed, 2);
    assert!(
        errors[0].message.contains("{x0, x1, id, amount}"),
        "{errors:?}"
    );
    assert!(errors[1].message.contains("{y0, id, amount}"), "{errors:?}");
}

#[test]
fn test_should_resolve_a_relative_import_and_a_package_init() {
    let main = "import pandas as pd\nfrom pkg.sub import METRICS\nfrom pkg import BASE\n\na = pd.read_csv(\"f.csv\", usecols=METRICS)\nb = pd.read_csv(\"g.csv\", usecols=BASE)\nprint(a[\"zz\"])\nprint(b[\"zz\"])\n";
    let (linter, errors) = check_in_project(
        &[
            ("pkg/__init__.py", "BASE = [\"p\", \"q\"]\n"),
            ("pkg/sub.py", "from .helpers import X\nMETRICS = [\"s\"]\n"),
            ("pkg/helpers.py", "X = [\"h\"]\n"),
        ],
        main,
    );
    assert_eq!(linter.dataframes_typed, 2, "errors: {errors:?}");

    let relative = "import pandas as pd\nfrom .helpers import X\n\ndf = pd.read_csv(\"f.csv\", usecols=X)\nprint(df[\"zz\"])\n";
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(root.join("pkg/__init__.py"), "").unwrap();
    fs::write(root.join("pkg/helpers.py"), "X = [\"h\"]\n").unwrap();
    fs::write(root.join("pkg/use.py"), relative).unwrap();
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let path = root.join("pkg/use.py");
    linter.load_cross_file_symbols(&index, relative, &path, root);
    let errors = linter.check_file_internal(relative, &path).unwrap();
    assert_eq!(linter.dataframes_typed, 1);
    assert!(errors[0].message.contains("{h}"), "{errors:?}");
}

#[test]
fn test_should_not_trust_an_imported_value_it_cannot_prove_constant() {
    for config in [
        // mutated in its own module
        "COLS = [\"a\"]\nCOLS.append(\"b\")\n",
        // rebound
        "COLS = [\"a\"]\nCOLS = [\"b\"]\n",
        // built from the environment
        "import os\nCOLS = os.environ[\"C\"].split(\",\")\n",
        // module namespace is dynamic
        "COLS = [\"a\"]\nglobals()\n",
    ] {
        let main = "import pandas as pd\nfrom config import COLS\n\ndf = pd.read_csv(\"f.csv\", usecols=COLS)\n";
        let (linter, _) = check_in_project(&[("config.py", config)], main);
        assert_eq!(linter.dataframes_typed, 0, "{config}");
    }
}

#[test]
fn test_should_not_let_the_importer_alias_or_mutate_an_imported_list() {
    for tail in [
        "COLS.append(\"z\")",
        "alias = COLS",
        "helper(COLS)",
        "COLS[0] = \"z\"",
    ] {
        let main = format!("import pandas as pd\nfrom config import COLS\n\n{tail}\ndf = pd.read_csv(\"f.csv\", usecols=COLS)\n");
        let (linter, _) = check_in_project(&[("config.py", "COLS = [\"a\"]\n")], &main);
        assert_eq!(linter.dataframes_typed, 0, "{tail}");
    }
}

#[test]
fn test_should_not_evaluate_an_imported_function_that_depends_on_the_importer() {
    // `helper` in config.py reads a local that clashes with a module constant, and one
    // function reads a name that only the importing file defines
    let config = "BASE = [\"id\"]\n\n\ndef clash():\n    out = BASE + [\"x\"]\n    BASE = out\n    return BASE\n\n\ndef reads_importer():\n    return LOCAL + [\"y\"]\n\n\ndef loop():\n    for _ in range(1):\n        pass\n    return [\"z\"]\n";
    let main = "import pandas as pd\nfrom config import clash, reads_importer, loop\n\nLOCAL = [\"l\"]\na = pd.read_csv(\"a.csv\", usecols=clash())\nb = pd.read_csv(\"b.csv\", usecols=reads_importer())\nc = pd.read_csv(\"c.csv\", usecols=loop())\n";
    let (linter, _) = check_in_project(&[("config.py", config)], main);
    assert_eq!(linter.dataframes_typed, 0);
}

#[test]
fn test_should_not_evaluate_an_imported_function_whose_module_rebinds_a_builtin() {
    let config = "len = lambda x: 99\n\n\ndef cols():\n    return [\"a\"] * len([1])\n";
    let main = "import pandas as pd\nfrom config import cols\n\ndf = pd.read_csv(\"f.csv\", usecols=cols())\n";
    assert_eq!(
        check_in_project(&[("config.py", config)], main)
            .0
            .dataframes_typed,
        0
    );
}

#[test]
fn test_should_not_resolve_a_name_imported_from_outside_the_project() {
    let main = "import pandas as pd\nfrom elsewhere import COLS\n\ndf = pd.read_csv(\"f.csv\", usecols=COLS)\n";
    assert_eq!(check_in_project(&[], main).0.dataframes_typed, 0);
}

#[test]
fn test_should_apply_an_imported_helpers_edits() {
    // arrange / act
    let (linter, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nfrom helpers import add, clean, fix_in_place\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = clean(df)\ndf = add(df)\nfix_in_place(df)\nprint(df[\"z\"], df[\"y\"])\nprint(df[\"nope\"])\n",
        );

    // assert: `z` and `y` exist, `nope` does not, and no unverifiable noise.
    assert_eq!(linter.dataframes_total, 3);
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_apply_a_helper_imported_under_an_alias() {
    // arrange / act
    let (_, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nfrom helpers import add as add_z\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = add_z(df)\nprint(df[\"z\"])\nprint(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_apply_a_helper_reached_through_a_module_import() {
    // arrange / act
    let (_, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nimport helpers\nimport helpers as h\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = helpers.add(df)\nh.fix_in_place(df)\nprint(df[\"z\"], df[\"y\"])\nprint(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_not_treat_a_reshaping_imported_helper_as_a_passthrough() {
    // arrange / act
    let (_, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nfrom helpers import reshape\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = reshape(df)\nprint(df[\"zzz\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_not_apply_a_module_helper_that_was_not_imported_by_that_name() {
    // arrange / act: only `clean` is imported by name, so a bare `add(df)` is not it.
    let (_, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nfrom helpers import clean\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = add(df)\nprint(df[\"z\"])\n",
        );

    // assert: `add` is unknown here, so the frame is unresolved, not `{a, z}`.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}

#[test]
fn test_should_let_a_local_definition_win_over_an_imported_helper() {
    // arrange / act
    let (_, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nfrom helpers import add\n\n\ndef add(df):\n    return df.dropna()\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = add(df)\nprint(df[\"z\"])\n",
        );

    // assert: the local `add` adds nothing.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
}

#[test]
fn test_should_resolve_a_local_helper_that_delegates_to_an_imported_one() {
    // arrange / act
    let (_, errors) = check_against_helpers(
            HELPERS,
            "import pandas as pd\nfrom helpers import fix_in_place\n\n\ndef normalize(df):\n    fix_in_place(df)\n\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nnormalize(df)\nprint(df[\"y\"])\nprint(df[\"nope\"])\n",
        );

    // assert
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_resolve_a_delegate_to_a_helper_imported_into_the_defining_file() {
    // arrange: main.py imports `normalize` from middle.py, whose own body delegates
    // to `add`, imported (into middle.py) from base.py -- neither file this file's
    // own indexing pass has any visibility into on its own.
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
        "from base import add\n\n\ndef normalize(df):\n    add(df)\n    log(df)\n",
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

    // assert: `z` reached `df` through two import hops; `log(df)`, resolving nowhere
    // in the project, contributed nothing, and did not make anything unresolved.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'nope'"));
}

#[test]
fn test_should_resolve_a_returned_delegate_across_two_files() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("base.py"),
        "def add(df):\n    df = df.copy()\n    df[\"z\"] = 1\n    return df\n",
    )
    .unwrap();
    fs::write(
        root.join("middle.py"),
        "from base import add\n\n\ndef normalize(df):\n    return add(df)\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom middle import normalize\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nout = normalize(df)\nprint(out[\"z\"])\nprint(df[\"z\"])\n";
    fs::write(root.join("main.py"), main_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();

    // assert: `out` has `z` (added on a copy inside `add`), `df` does not.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("'z'"));
}

#[test]
fn test_should_not_treat_a_delegate_to_an_unresolvable_return_as_a_passthrough_across_files() {
    // arrange: `normalize` (in middle.py) returns the result of a name main.py's
    // indexing pass has never heard of at all.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("middle.py"),
        "def normalize(df):\n    return opaque_transform(df)\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom middle import normalize\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\ndf = normalize(df)\nprint(df[\"zzz\"])\n";
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
fn test_should_not_loop_on_a_cross_file_delegation_cycle() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "").unwrap();
    fs::write(
        root.join("a.py"),
        "from b import pong\n\n\ndef ping(df):\n    df[\"z\"] = 1\n    pong(df)\n",
    )
    .unwrap();
    fs::write(
        root.join("b.py"),
        "from a import ping\n\n\ndef pong(df):\n    ping(df)\n",
    )
    .unwrap();
    let main_source = "import pandas as pd\nfrom a import ping\n\ndf = pd.read_csv(\"a.csv\", usecols=[\"a\"])\nping(df)\nprint(df[\"zzz\"])\n";
    fs::write(root.join("main.py"), main_source).unwrap();

    // act
    let index = build_index_internal(root);
    let mut linter = Linter::new();
    let main_path = root.join("main.py");
    linter.load_cross_file_symbols(&index, main_source, &main_path, root);
    let errors = linter.check_file_internal(main_source, &main_path).unwrap();

    // assert: the cycle makes the columns unknown rather than hanging.
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0].code, CODE_UNVERIFIABLE_COLUMN);
}
