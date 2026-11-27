use super::*;

// A package whose modules all re-export names from many others used to be walked once per
// path to each module (about 24^4 times here), which never finished on real packages.
#[test]
fn test_should_check_a_densely_cross_reexporting_package_within_a_hard_time_cap() {
    // arrange
    const MODULES: usize = 40;
    const REEXPORTS_PER_MODULE: usize = 24;
    let project = Project::new();
    for i in 0..MODULES {
        let mut text = format!("c{i} = \"col_{i}\"\n");
        for k in 1..=REEXPORTS_PER_MODULE {
            let j = (i + k) % MODULES;
            text.push_str(&format!("from .m{j:02} import c{j}\n"));
        }
        project.write(&format!("pkg/m{i:02}.py"), &text);
    }
    project.write("pkg/__init__.py", "from .m00 import c0\n");
    project.write(
        "pkg/main.py",
        "import pandas as pd\nfrom pkg.m00 import c0\n\ndf = pd.read_csv(\"x.csv\", usecols=[\"col_0\"])\nprint(df[c0])\n",
    );

    // act
    let mut child = Command::new(BIN)
        .args(["check", "--no-cache", "."])
        .current_dir(project.path())
        .env("TYPEDFRAMES_CACHE_DIR", project.cache.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    // assert
    let status = status.expect("check did not finish within 20s: re-export resolution blew up");
    assert_eq!(Some(0), status.code());
}

// Each once-read column name was compared with every literal in the file, so a class with
// thousands of methods took seconds; the work per file is now bounded.
#[test]
fn test_should_check_a_class_with_thousands_of_methods_within_a_hard_time_cap() {
    // arrange
    const METHODS: usize = 3000;
    let project = Project::new();
    let mut text = String::from("import pandas as pd\n\n\nclass Wide:\n");
    for i in 0..METHODS {
        text.push_str(&format!(
            "    def m{i}(self, df):\n        df = df.drop(columns=[\"x{i}\"])\n        return df[\"y{i}\"]\n\n"
        ));
    }
    project.write("wide.py", &text);

    // act
    let mut child = Command::new(BIN)
        .args(["check", "--no-cache", "."])
        .current_dir(project.path())
        .env("TYPEDFRAMES_CACHE_DIR", project.cache.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    // assert
    assert!(
        status.is_some(),
        "check did not finish within 20s: work grew faster than the file"
    );
}
