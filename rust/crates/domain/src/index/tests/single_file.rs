use super::*;

#[test]
fn test_should_resolve_a_schema_two_import_hops_away_from_a_single_checked_file() {
    // arrange: pipeline.py imports load_users from loaders.py, whose return
    // annotation names UserSchema -- defined in a THIRD file pipeline.py never
    // mentions. Following only direct imports would miss it.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let pipeline_path = write_two_hop_project(root);

    // act
    let index = build_single_file_index_internal(&pipeline_path).expect("project root found");

    // assert: the two-hop schema resolved, with the location that makes the
    // "(defined at ...)" part of a diagnostic possible.
    assert_eq!(
        index.all_schemas.get("UserSchema"),
        Some(&vec!["name".to_string(), "user_id".to_string()])
    );
    assert!(index.all_schema_locations.contains_key("UserSchema"));
}

#[test]
fn test_should_not_resolve_a_schema_name_two_packages_define_differently() {
    // arrange
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let schema = |extra: &str| {
        format!(
                "from typedframes import BaseSchema, Column\n\n\nclass TestRaw(BaseSchema):\n    uid = Column()\n{extra}\n\n\nclass Same(BaseSchema):\n    a = Column()\n"
            )
    };
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    fs::write(root.join("a/schema.py"), schema("    x1 = Column()")).unwrap();
    fs::write(root.join("b/schema.py"), schema("    Age = Column()")).unwrap();

    // act
    let index = build_index_internal(root);

    // assert
    assert!(!index.all_schemas.contains_key("TestRaw"));
    assert!(!index.all_schema_locations.contains_key("TestRaw"));
    assert!(index.all_schemas.contains_key("Same"));
}

#[test]
fn test_should_not_index_project_files_the_checked_file_never_references() {
    // The whole point of single-file mode: cost proportional to what the checked
    // file references, not to what the project contains. An unrelated file sitting
    // right next to pipeline.py must never be opened.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let pipeline_path = write_two_hop_project(root);
    fs::write(
        root.join("unrelated.py"),
        r#"
from typedframes import BaseSchema, Column


class UnrelatedSchema(BaseSchema):
    other = Column(type=int)
"#,
    )
    .unwrap();

    // act
    let index = build_single_file_index_internal(&pipeline_path).expect("project root found");

    // assert
    let indexed: Vec<&String> = index.files.keys().collect();
    assert_eq!(indexed.len(), 3, "{indexed:#?}");
    assert!(
        !indexed.iter().any(|p| p.ends_with("unrelated.py")),
        "unrelated.py is not referenced by pipeline.py and must not be indexed: {indexed:#?}"
    );
    assert!(!index.all_schemas.contains_key("UnrelatedSchema"));
    // ... and for contrast, the whole-project index DOES pick it up -- the two
    // modes differ in file set, which is exactly the intended trade-off.
    assert!(build_index_internal(root)
        .all_schemas
        .contains_key("UnrelatedSchema"));
}

#[test]
fn test_should_stop_following_a_single_file_import_chain_past_the_depth_bound() {
    // arrange: a straight chain hop0 (the checked file) -> hop1 -> ... -> hop5.
    // MAX_SINGLE_FILE_IMPORT_DEPTH hops past the checked file get indexed; the
    // next one does not.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    let chain_len = MAX_SINGLE_FILE_IMPORT_DEPTH + 3;
    for i in 0..chain_len {
        fs::write(
            root.join(format!("hop{i}.py")),
            format!("from hop{} import thing_{}\n", i + 1, i + 1),
        )
        .unwrap();
    }
    fs::write(
        root.join(format!("hop{chain_len}.py")),
        format!("thing_{chain_len} = 1\n"),
    )
    .unwrap();

    // act
    let index = build_single_file_index_internal(&root.join("hop0.py")).expect("root found");

    // assert
    let indexed: Vec<&String> = index.files.keys().collect();
    assert_eq!(
        indexed.len(),
        MAX_SINGLE_FILE_IMPORT_DEPTH + 1,
        "{indexed:#?}"
    );
    let last_followed = format!("hop{MAX_SINGLE_FILE_IMPORT_DEPTH}.py");
    let first_dropped = format!("hop{}.py", MAX_SINGLE_FILE_IMPORT_DEPTH + 1);
    assert!(indexed.iter().any(|p| p.ends_with(&last_followed)));
    assert!(!indexed.iter().any(|p| p.ends_with(&first_dropped)));
}

#[test]
fn test_should_cap_the_number_of_files_a_single_file_import_walk_indexes() {
    // arrange: one file importing far more modules than the cap allows, so the
    // depth bound alone never fires -- only MAX_SINGLE_FILE_INDEXED_FILES stops it.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    let fan_out = MAX_SINGLE_FILE_INDEXED_FILES + 50;
    let mut imports = String::new();
    for i in 0..fan_out {
        fs::write(
            root.join(format!("wide_{i}.py")),
            format!("value_{i} = 1\n"),
        )
        .unwrap();
        imports.push_str(&format!("from wide_{i} import value_{i}\n"));
    }
    let hub_path = root.join("hub.py");
    fs::write(&hub_path, imports).unwrap();

    // act
    let index = build_single_file_index_internal(&hub_path).expect("root found");

    // assert
    assert_eq!(index.files.len(), MAX_SINGLE_FILE_INDEXED_FILES);
}

#[test]
fn test_should_follow_a_single_file_import_into_a_src_layout() {
    // A `src/` layout is the second root resolve_module_file tries at check time,
    // so single-file discovery has to try it too or the file it finds and the file
    // check time looks for would disagree.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column


class SrcSchema(BaseSchema):
    amount = Column(type=int)
"#,
    )
    .unwrap();
    let pipeline_path = root.join("src").join("pipeline.py");
    fs::write(&pipeline_path, "from schemas import SrcSchema\n").unwrap();

    // act
    let index = build_single_file_index_internal(&pipeline_path).expect("root found");

    // assert
    assert!(index.all_schemas.contains_key("SrcSchema"));
}

#[test]
fn test_should_not_follow_a_single_file_import_into_an_excluded_directory() {
    // A configured `exclude` prunes the same directories here as it does for the
    // whole-project walk -- an import that happens to resolve into one is dropped
    // rather than quietly reintroducing what the user asked to ignore.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\nexclude = [\"vendor\"]\n",
    )
    .unwrap();
    fs::create_dir(root.join("vendor")).unwrap();
    fs::write(
        root.join("vendor").join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column


class VendoredSchema(BaseSchema):
    x = Column(type=int)
"#,
    )
    .unwrap();
    let pipeline_path = root.join("pipeline.py");
    fs::write(
        &pipeline_path,
        "from vendor.schemas import VendoredSchema\n",
    )
    .unwrap();

    // act
    let index = build_single_file_index_internal(&pipeline_path).expect("root found");

    // assert
    assert_eq!(index.files.len(), 1, "only pipeline.py itself");
    assert!(!index.all_schemas.contains_key("VendoredSchema"));
}

#[test]
fn test_should_auto_discover_an_external_package_from_a_single_checked_file() {
    // arrange: the same shape as linter.rs's py.typed auto-discovery coverage, but
    // reached from ONE file rather than a whole-project walk -- the import sits
    // right there in the checked file, so it must still be followed.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    let pkg_dir = site_packages.join("internal_repo_pkg");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(pkg_dir.join("py.typed"), "").unwrap();
    fs::write(
        pkg_dir.join("__init__.py"),
        r#"
import pandas as pd


class DataRepository:
    def get(self, query: str) -> pd.DataFrame:
        return pd.read_sql(query, None)
"#,
    )
    .unwrap();
    let pipeline_path = root.join("pipeline.py");
    fs::write(
        &pipeline_path,
        r#"
from internal_repo_pkg import DataRepository


class Pipeline:
    def __init__(self):
        self._data_repository = DataRepository()

    def run(self):
        df = self._data_repository.get("SELECT * FROM training")
        return df
"#,
    )
    .unwrap();

    // act
    let index = build_single_file_index_internal(&pipeline_path).expect("root found");

    // assert: the installed package's own file was indexed alongside the checked
    // one, which is what makes DataRepository.get's return type resolvable.
    let indexed: Vec<&String> = index.files.keys().collect();
    assert!(
        indexed
            .iter()
            .any(|p| p.contains("internal_repo_pkg") && p.ends_with("__init__.py")),
        "{indexed:#?}"
    );
    let pkg_entry = index
        .files
        .get(pkg_dir.join("__init__.py").to_str().unwrap())
        .expect("package indexed");
    assert!(pkg_entry.class_methods.contains_key("DataRepository"));
}

#[test]
fn test_should_index_a_lone_script_s_sibling_imports_without_a_project_root() {
    // arrange: no pyproject.toml; Python would import `schemas` from the script's own
    // directory, and `cols` relative to the package.
    let dir = tempfile::tempdir().unwrap();
    let lone = dir.path().join("lone_script.py");
    fs::write(&lone, "from schemas import A\nfrom pkg.use import B\n").unwrap();
    fs::write(dir.path().join("schemas.py"), "A = 'a'\n").unwrap();
    std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
    fs::write(dir.path().join("pkg/__init__.py"), "").unwrap();
    fs::write(dir.path().join("pkg/use.py"), "from .cols import B\n").unwrap();
    fs::write(dir.path().join("pkg/cols.py"), "B = 'b'\n").unwrap();

    // act
    let index = build_single_file_index_internal(&lone).expect("indexed");

    // assert
    for file in ["schemas.py", "pkg/use.py", "pkg/cols.py"] {
        let path = dir.path().join(file);
        assert!(index.files.contains_key(path.to_str().unwrap()), "{file}");
    }
}

#[test]
fn test_should_resolve_a_param_governed_call_site_in_single_file_mode() {
    // The call-site pass (resolve_param_governed_call_sites) is a fold over the
    // file map it's handed, so it works unchanged on a single file's import
    // closure -- here the governed callee lives one hop away in helpers.py.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    fs::write(
        root.join("helpers.py"),
        r#"
from feast import FeatureStore
import pandas as pd


def load_conv_rate(store: FeatureStore, entity_df: pd.DataFrame, feature_names: list) -> None:
    df = store.get_historical_features(entity_df=entity_df, features=feature_names).to_df()
    print(df["conv_rate"])
"#,
    )
    .unwrap();
    let pipeline_path = root.join("pipeline.py");
    fs::write(
        &pipeline_path,
        r#"
from helpers import load_conv_rate

load_conv_rate(store, entity_df, ["driver_stats:acc_rate"])
"#,
    )
    .unwrap();

    // act
    let index = build_single_file_index_internal(&pipeline_path).expect("root found");

    // assert
    let errors = index
        .call_site_errors
        .get(pipeline_path.to_str().unwrap())
        .cloned()
        .unwrap_or_default();
    assert_eq!(errors.len(), 1, "errors: {errors:#?}");
    assert_eq!(errors[0].code, CODE_UNKNOWN_COLUMN);
    assert!(errors[0].message.contains("conv_rate"));
}
