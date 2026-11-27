use super::*;

#[test]
#[ignore]
fn bench_build_index_without_venv() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_import_heavy_project(root, 300);

    let start = std::time::Instant::now();
    let index = build_index_internal(root);
    let elapsed = start.elapsed();
    eprintln!(
        "build_index_internal, 300 files, no .venv: {:?} ({} files indexed)",
        elapsed,
        index.files.len()
    );
}

#[test]
#[ignore]
fn bench_build_index_with_venv_present_but_unused() {
    // Same project, but with a real (populated-looking) .venv present --
    // trace_external_packages is NOT set, so no external package should actually
    // be indexed, but every import resolution still probes for a site-packages
    // dir (cached after the first call -- this measures whether that caching
    // actually keeps the cost negligible for the common case where the feature
    // isn't in use at all).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_import_heavy_project(root, 300);
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    std::fs::create_dir_all(&site_packages).unwrap();
    // A handful of unrelated installed packages, so the directory listing this
    // benchmark exercises isn't unrealistically empty.
    for pkg in ["numpy", "pandas", "requests", "urllib3", "certifi"] {
        std::fs::create_dir_all(site_packages.join(pkg)).unwrap();
        fs::write(site_packages.join(pkg).join("__init__.py"), "").unwrap();
    }

    let start = std::time::Instant::now();
    let index = build_index_internal(root);
    let elapsed = start.elapsed();
    eprintln!(
        "build_index_internal, 300 files, .venv present (unused): {:?} ({} files indexed)",
        elapsed,
        index.files.len()
    );
}

#[test]
#[ignore]
fn bench_build_index_with_venv_and_allowlisted_package() {
    // Same as above, but trace_external_packages actually names one of the fake
    // installed packages -- measures the added cost of actually indexing an
    // external package's files, not just probing for the directory.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_import_heavy_project(root, 300);
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    std::fs::create_dir_all(&site_packages).unwrap();
    for pkg in ["numpy", "pandas", "requests", "urllib3", "certifi"] {
        std::fs::create_dir_all(site_packages.join(pkg)).unwrap();
        fs::write(site_packages.join(pkg).join("__init__.py"), "").unwrap();
    }
    let internal_pkg = site_packages.join("internal_snowflake_pkg");
    std::fs::create_dir_all(&internal_pkg).unwrap();
    fs::write(
        internal_pkg.join("__init__.py"),
        r#"
import pandas as pd

def load_orders(query: str) -> pd.DataFrame:
    return pd.read_csv(query, usecols=["order_id", "amount"])
"#,
    )
    .unwrap();
    fs::write(
        root.join("pyproject.toml"),
        "[tool.typedframes]\ntrace_external_packages = [\"internal_snowflake_pkg\"]\n",
    )
    .unwrap();

    let start = std::time::Instant::now();
    let index = build_index_internal(root);
    let elapsed = start.elapsed();
    eprintln!(
        "build_index_internal, 300 files, .venv + allowlisted package: {:?} ({} files indexed)",
        elapsed,
        index.files.len()
    );
}

#[test]
#[ignore]
fn bench_single_file_vs_whole_project_index() {
    // The trade-off single-file mode exists to make: the same project, indexed
    // whole vs. indexed from one file's own import closure. mod_0.py imports
    // exactly one hub module, so its closure is 2 files out of 305.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_import_heavy_project(root, 300);

    let start = std::time::Instant::now();
    let whole = build_index_internal(root);
    let whole_elapsed = start.elapsed();

    let target = root.join("mod_0.py");
    let start = std::time::Instant::now();
    let single = build_single_file_index_internal(&target).expect("root found");
    let single_elapsed = start.elapsed();

    eprintln!(
        "305-file project: whole-project index {:?} ({} files) vs single-file index \
             {:?} ({} files)",
        whole_elapsed,
        whole.files.len(),
        single_elapsed,
        single.files.len()
    );
}

#[test]
#[ignore]
fn bench_single_file_index_worst_case_fan_out() {
    // The honest worst case for single-file mode: a file that imports enough
    // distinct modules, each importing more, to hit MAX_SINGLE_FILE_INDEXED_FILES.
    // Bounded by construction, but this is what "bounded" actually costs.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    let fan_out = MAX_SINGLE_FILE_INDEXED_FILES + 50;
    let mut imports = String::new();
    for i in 0..fan_out {
        fs::write(
            root.join(format!("wide_{i}.py")),
            format!(
                r#"
import pandas as pd

def load_{i}(path: str) -> pd.DataFrame:
    return pd.read_csv(path, usecols=["a", "b", "c"])
"#
            ),
        )
        .unwrap();
        imports.push_str(&format!("from wide_{i} import load_{i}\n"));
    }
    let hub_path = root.join("hub.py");
    fs::write(&hub_path, imports).unwrap();

    let start = std::time::Instant::now();
    let index = build_single_file_index_internal(&hub_path).expect("root found");
    let elapsed = start.elapsed();
    eprintln!(
        "single-file index, worst-case fan-out (capped): {:?} ({} files indexed of \
             {fan_out} importable)",
        elapsed,
        index.files.len()
    );
}

#[test]
#[ignore]
fn bench_find_site_packages_dir_repeated_calls_uncached() {
    // Directly measures what EVERY resolve_module_file call used to pay before
    // SITE_PACKAGES_CACHE was added -- one read_dir per call, simulating roughly
    // one import statement's worth of resolution per iteration.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let site_packages = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    std::fs::create_dir_all(&site_packages).unwrap();
    for pkg in ["numpy", "pandas", "requests", "urllib3", "certifi"] {
        std::fs::create_dir_all(site_packages.join(pkg)).unwrap();
    }

    let iterations = 1000;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let _ = find_site_packages_dir_uncached(&OsFiles, root);
    }
    let elapsed = start.elapsed();
    eprintln!(
        "find_site_packages_dir_uncached x{iterations}: {:?} total, {:?}/call",
        elapsed,
        elapsed / iterations
    );

    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let _ = find_site_packages_dir(&OsFiles, root);
    }
    let elapsed = start.elapsed();
    eprintln!(
        "find_site_packages_dir (cached) x{iterations}: {:?} total, {:?}/call",
        elapsed,
        elapsed / iterations
    );
}
