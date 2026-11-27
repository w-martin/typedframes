use super::*;

#[test]
fn test_should_prune_configured_exclude_directories_from_project_index() {
    // A non-default directory (e.g. a vendored `third_party/`) isn't caught by
    // DEFAULT_EXCLUDED_DIRS -- [tool.typedframes] exclude is what lets a project
    // prune it (or any other named directory) explicitly.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\nexclude = [\"third_party\"]",
    )
    .unwrap();
    fs::create_dir(root.join("third_party")).unwrap();
    fs::write(
        root.join("third_party").join("vendored.py"),
        r#"
from typedframes import BaseSchema, Column

class VendoredSchema(BaseSchema):
    x = Column(type=int)
"#,
    )
    .unwrap();
    fs::write(root.join("app.py"), "x = 1\n").unwrap();

    // act
    let index = build_index_internal(root);

    // assert -- only app.py was indexed; third_party/ was pruned entirely
    let indexed: Vec<&String> = index.files.keys().collect();
    assert!(
        indexed.iter().any(|p| p.ends_with("app.py")),
        "{indexed:#?}"
    );
    assert!(
        !indexed.iter().any(|p| p.contains("third_party")),
        "third_party/ should have been pruned: {indexed:#?}"
    );
    assert!(!index.all_schemas.contains_key("VendoredSchema"));
}

#[test]
fn test_should_prune_a_rust_target_directory_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("rust").join("target").join("debug")).unwrap();
    fs::write(
        root.join("rust")
            .join("target")
            .join("debug")
            .join("generated.py"),
        "x = 1\n",
    )
    .unwrap();
    fs::write(root.join("app.py"), "x = 1\n").unwrap();

    // act
    let index = build_index_internal(root);

    // assert
    let indexed: Vec<&String> = index.files.keys().collect();
    assert!(
        indexed.iter().any(|p| p.ends_with("app.py")),
        "{indexed:#?}"
    );
    assert!(
        !indexed.iter().any(|p| p.ends_with("generated.py")),
        "{indexed:#?}"
    );
}

#[test]
fn test_should_prune_dot_claude_by_default_with_no_config_at_all() {
    // .claude is in DEFAULT_EXCLUDED_DIRS -- pruned automatically with no
    // [tool.typedframes] exclude configured at all, same as .venv/.git/etc.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".claude").join("worktrees").join("agent-1")).unwrap();
    fs::write(
        root.join(".claude")
            .join("worktrees")
            .join("agent-1")
            .join("stale.py"),
        r#"
from typedframes import BaseSchema, Column

class StaleSchema(BaseSchema):
    x = Column(type=int)
"#,
    )
    .unwrap();
    fs::write(root.join("app.py"), "x = 1\n").unwrap();

    // act
    let index = build_index_internal(root);

    // assert
    let indexed: Vec<&String> = index.files.keys().collect();
    assert!(
        indexed.iter().any(|p| p.ends_with("app.py")),
        "{indexed:#?}"
    );
    assert!(
        !indexed.iter().any(|p| p.contains(".claude")),
        ".claude/ should have been pruned by default: {indexed:#?}"
    );
    assert!(!index.all_schemas.contains_key("StaleSchema"));
}

#[test]
fn test_should_replace_rather_than_add_to_default_excludes_when_configured() {
    // Configuring [tool.typedframes] exclude REPLACES DEFAULT_EXCLUDED_DIRS
    // entirely -- it does not add to it. A project that configures its own
    // exclude list without re-listing .venv gets .venv walked again; that's the
    // deliberate override semantics (matching ruff's own exclude, as opposed to
    // extend-exclude), not a bug.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\nexclude = [\"custom_dir\"]",
    )
    .unwrap();
    fs::create_dir(root.join("custom_dir")).unwrap();
    fs::write(root.join("custom_dir").join("skipped.py"), "x = 1\n").unwrap();
    fs::create_dir(root.join(".venv")).unwrap();
    fs::write(
        root.join(".venv").join("walked.py"),
        r#"
from typedframes import BaseSchema, Column

class VenvSchema(BaseSchema):
    x = Column(type=int)
"#,
    )
    .unwrap();

    // act
    let index = build_index_internal(root);

    // assert
    let indexed: Vec<&String> = index.files.keys().collect();
    assert!(
        !indexed.iter().any(|p| p.contains("custom_dir")),
        "custom_dir/ should have been pruned (in the configured exclude list): {indexed:#?}"
    );
    assert!(
        indexed.iter().any(|p| p.contains(".venv")),
        ".venv/ should NOT be pruned once exclude is configured without re-listing \
             it -- override, not union: {indexed:#?}"
    );
    assert!(index.all_schemas.contains_key("VenvSchema"));
}
