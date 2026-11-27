use super::*;
use std::fs;

// A working copy that refuses to write one file.
struct RefusingOne(PathBuf);

impl Workspace for RefusingOne {
    fn is_tracked(&self, _path: &Path) -> bool {
        true
    }
    fn write_atomically(&self, path: &Path, content: &str) -> std::io::Result<()> {
        if path == self.0 {
            return Err(std::io::Error::other("refused"));
        }
        typedframes_adapters::GitWorkspace.write_atomically(path, content)
    }
}

fn files() -> Arc<dyn FileSystem> {
    Arc::new(typedframes_adapters::OsFiles)
}

const SAFE: &str = "import pandas as pd\n\n\ndef run():\n    df = pd.read_json(\"x.json\")\n    return df[\"a\"] + df[\"b\"]\n";

// read only on one path, so a review-tier suggestion
const REVIEW: &str = "import pandas as pd\n\n\ndef run(flag):\n    df = pd.read_json(\"x.json\")\n    if flag:\n        return df[\"b\"]\n    return df[\"a\"]\n";

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pyproject.toml"), "").unwrap();
    for (name, source) in files {
        fs::write(dir.path().join(name), source).unwrap();
    }
    dir
}

fn request_in(dir: &Path, name: &str) -> (String, String, Selection) {
    (
        dir.join(name).to_string_lossy().into_owned(),
        name.to_string(),
        Selection::SafeTier,
    )
}

fn request(
    dir: &tempfile::TempDir,
    name: &str,
    selection: Selection,
) -> (String, String, Selection) {
    (
        dir.path().join(name).to_string_lossy().into_owned(),
        name.to_string(),
        selection,
    )
}

fn read(dir: &tempfile::TempDir, name: &str) -> String {
    fs::read_to_string(dir.path().join(name)).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

const NOTEBOOK: &str = r##"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": 3,
   "id": "aaaaaaaa",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "kept\n"
     ]
    }
   ],
   "source": [
    "import pandas as pd\n"
   ]
  },
  {
   "cell_type": "markdown",
   "id": "bbbbbbbb",
   "metadata": {},
   "source": [
    "Between the code cells"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "id": "cccccccc",
   "metadata": {},
   "outputs": [],
   "source": [
    "class Base:\n",
    "    TARGET = \"y\"\n",
    "\n",
    "\n",
    "class Repo(Base):\n",
    "    A: str = \"a\"\n",
    "\n",
    "    def load(self):\n",
    "        return pd.DataFrame(columns=[self.A, self.TARGET])"
   ]
  }
 ],
 "metadata": {
  "kernelspec": {
   "display_name": "Python 3",
   "language": "python",
   "name": "python3"
  }
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;

#[test]
fn test_should_apply_the_safe_tier_and_write_the_file() {
    let dir = project(&[("a.py", SAFE)]);

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::SafeTier)],
        None,
        false,
    );

    assert!(result.failed.is_none());
    assert!(result.files[0].written);
    assert_eq!(result.files[0].applied.len(), 1);
    assert_eq!(
        read(&dir, "a.py"),
        SAFE.replace("    return", "    df = df[[\"a\", \"b\"]]\n    return")
    );
}

#[test]
fn test_should_not_apply_a_review_suggestion_unless_it_was_chosen() {
    let dir = project(&[("a.py", REVIEW)]);

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::SafeTier)],
        None,
        false,
    );

    assert!(!result.files[0].written);
    assert_eq!(read(&dir, "a.py"), REVIEW);
}

#[test]
fn test_should_apply_a_review_suggestion_that_was_chosen_by_position() {
    let dir = project(&[("a.py", REVIEW)]);
    let origin = (5, 5);

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::Origins(vec![origin]))],
        None,
        false,
    );

    assert!(result.files[0].written, "{:?}", result.files[0].skipped);
    assert!(read(&dir, "a.py").contains("    df = df[[\"b\", \"a\"]]\n"));
}

#[test]
fn test_should_skip_a_chosen_suggestion_that_no_longer_exists() {
    let dir = project(&[("a.py", SAFE)]);

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::Origins(vec![(99, 1)]))],
        None,
        false,
    );

    assert!(!result.files[0].written);
    assert!(result.files[0].skipped[0].reason.contains("changed since"));
    assert_eq!(read(&dir, "a.py"), SAFE);
}

#[test]
fn test_should_not_apply_the_same_suggestion_twice() {
    let dir = project(&[("a.py", SAFE)]);
    apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::SafeTier)],
        None,
        false,
    );
    let once = read(&dir, "a.py");

    let second = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::SafeTier)],
        None,
        false,
    );

    assert!(!second.files[0].written, "{:?}", second.files[0]);
    assert_eq!(read(&dir, "a.py"), once);
}

#[test]
fn test_should_apply_to_several_files_together() {
    let dir = project(&[("a.py", SAFE), ("b.py", SAFE)]);

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[
            request(&dir, "a.py", Selection::SafeTier),
            request(&dir, "b.py", Selection::SafeTier),
        ],
        None,
        false,
    );

    assert!(result.files.iter().all(|f| f.written));
    assert!(read(&dir, "a.py").contains("df[[\"a\", \"b\"]]"));
    assert!(read(&dir, "b.py").contains("df[[\"a\", \"b\"]]"));
}

#[test]
fn test_should_write_nothing_when_a_file_changed_since_it_was_analysed() {
    let dir = project(&[("a.py", SAFE)]);
    let path = dir.path().join("a.py");
    let plan = Planned {
        path: path.clone(),
        original: SAFE.to_string(),
        edited: format!("{SAFE}# edited\n"),
    };
    fs::write(&path, "# someone else's edit\n").unwrap();

    let result = commit(&*files(), &typedframes_adapters::GitWorkspace, &[plan]);

    assert!(result.unwrap_err().contains("changed on disk"));
    assert_eq!(read(&dir, "a.py"), "# someone else's edit\n");
}

#[test]
fn test_should_restore_every_file_when_a_later_write_fails() {
    let dir = project(&[("a.py", SAFE), ("b.py", SAFE)]);
    let plans = vec![
        Planned {
            path: dir.path().join("a.py"),
            original: SAFE.to_string(),
            edited: format!("{SAFE}# one\n"),
        },
        Planned {
            path: dir.path().join("b.py"),
            original: SAFE.to_string(),
            edited: format!("{SAFE}# two\n"),
        },
    ];
    let refusing = RefusingOne(dir.path().join("b.py"));

    let result = commit(&*files(), &refusing, &plans);

    assert!(result.is_err());
    assert_eq!(read(&dir, "a.py"), SAFE);
    assert_eq!(read(&dir, "b.py"), SAFE);
}

#[test]
fn test_should_leave_no_temporary_files_behind() {
    let dir = project(&[("a.py", SAFE)]);

    apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::SafeTier)],
        None,
        false,
    );

    let names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains("typedframes-")),
        "{names:?}"
    );
}

#[test]
fn test_should_refuse_to_edit_a_file_git_does_not_track() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pyproject.toml"), "").unwrap();
    fs::write(dir.path().join("tracked.py"), SAFE).unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "tracked.py"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    fs::write(dir.path().join("loose.py"), SAFE).unwrap();
    let requests = [
        request_in(dir.path(), "tracked.py"),
        request_in(dir.path(), "loose.py"),
    ];

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &requests,
        None,
        true,
    );

    assert!(result.files[0].written);
    assert!(!result.files[1].written);
    let error = result.files[1].error.as_deref().unwrap();
    assert!(error.contains("not tracked by git"), "{error}");
    assert_eq!(
        fs::read_to_string(dir.path().join("loose.py")).unwrap(),
        SAFE
    );
}

#[test]
fn test_should_edit_an_untracked_file_when_tracking_is_not_required() {
    let dir = project(&[("a.py", SAFE)]);

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "a.py", Selection::SafeTier)],
        None,
        false,
    );

    assert!(result.files[0].written);
}

#[test]
fn test_should_write_a_file_requested_twice_only_once() {
    let dir = project(&[("a.py", SAFE)]);
    let requests = [
        request(&dir, "a.py", Selection::SafeTier),
        request(&dir, "a.py", Selection::SafeTier),
    ];

    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &requests,
        None,
        false,
    );

    assert!(result.files[0].written);
    assert!(!result.files[1].written);
    assert!(result.files[1].error.is_some());
    assert_eq!(read(&dir, "a.py").matches("df = df[[").count(), 1);
}

#[test]
fn test_should_apply_a_suggestion_to_a_notebook_and_leave_everything_else() {
    // arrange
    let dir = project(&[("n.ipynb", NOTEBOOK)]);

    // act
    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "n.ipynb", Selection::SafeTier)],
        None,
        false,
    );

    // assert: the import joins cell 1, both constants change in cell 3, and the outputs,
    // markdown cell and ids are untouched.
    assert!(result.files[0].written, "{:?}", result.files[0]);
    assert_eq!(result.files[0].applied.len(), 2);
    let expected = NOTEBOOK
        .replace(
            "    \"import pandas as pd\\n\"\n",
            "    \"from typing import Final\\n\",\n    \"\\n\",\n    \"import pandas as pd\\n\"\n",
        )
        .replace("    TARGET = ", "    TARGET: Final = ")
        .replace("    A: str = ", "    A: Final[str] = ");
    assert_eq!(read(&dir, "n.ipynb"), expected);
}

#[test]
fn test_should_write_a_notebook_unchanged_when_nothing_is_suggested() {
    // arrange
    let dir = project(&[("n.ipynb", NOTEBOOK)]);
    let source = NOTEBOOK.replace("    TARGET = ", "    TARGET: Final = ");
    let source = source.replace("    A: str = ", "    A: Final[str] = ");
    fs::write(dir.path().join("n.ipynb"), &source).unwrap();

    // act
    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "n.ipynb", Selection::SafeTier)],
        None,
        false,
    );

    // assert
    assert!(!result.files[0].written);
    assert_eq!(read(&dir, "n.ipynb"), source);
}

#[test]
fn test_should_move_cell_boundaries_with_an_edit_that_changes_a_cells_length() {
    // arrange: the edit sits in the first cell, so every later cell shifts.
    let source = NOTEBOOK.replace(
        "    \"import pandas as pd\\n\"\n",
        "    \"A = 1\\n\",\n    \"import pandas as pd\\n\"\n",
    );
    let dir = project(&[("n.ipynb", &source)]);

    // act
    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "n.ipynb", Selection::SafeTier)],
        None,
        false,
    );

    // assert: still three cells, code split exactly as before.
    assert!(result.files[0].written, "{:?}", result.files[0]);
    let written: serde_json::Value = serde_json::from_str(&read(&dir, "n.ipynb")).unwrap();
    let cells = written["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 3);
    assert_eq!(cells[1]["cell_type"], "markdown");
    assert!(cells[2]["source"][0]
        .as_str()
        .unwrap()
        .starts_with("class Base"));
}

#[test]
fn test_should_keep_an_import_in_the_cell_that_ends_with_it() {
    // arrange: the last import is the last line of cell 1, which has no trailing newline.
    let source = NOTEBOOK.replace("    \"import pandas as pd\\n\"\n", "    \"import os\"\n");
    let dir = project(&[("n.ipynb", &source)]);

    // act
    let result = apply_all(
        &files(),
        &typedframes_adapters::GitWorkspace,
        &[request(&dir, "n.ipynb", Selection::SafeTier)],
        None,
        false,
    );

    // assert: cell 1 gains the import line, the markdown and class cells are unmoved.
    assert!(result.files[0].written, "{:?}", result.files[0]);
    let written: serde_json::Value = serde_json::from_str(&read(&dir, "n.ipynb")).unwrap();
    let cells = written["cells"].as_array().unwrap();
    assert_eq!(
        cells[0]["source"],
        serde_json::json!(["import os\n", "from typing import Final"])
    );
    assert_eq!(
        cells[1]["source"],
        serde_json::json!(["Between the code cells"])
    );
    assert!(cells[2]["source"][0]
        .as_str()
        .unwrap()
        .starts_with("class Base"));
}
