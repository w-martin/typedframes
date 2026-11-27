//! End-to-end tests of the `typedframes` binary: what a user sees and the exit status they get.

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

mod check;
mod constants;
mod helper_calls;
mod performance;
mod report;
mod schemas;
mod suggest;

use schemas::*;

const BIN: &str = env!("CARGO_BIN_EXE_typedframes");

const MISSING_COLUMN: &str = r#"from typing import Annotated

import polars as pl

from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    email = Column(type=str)


df: Annotated[pl.DataFrame, UserSchema] = pl.DataFrame({"user_id": [1], "email": ["a"]})
print(df["non_existent"])
"#;

const SAFE_SUGGESTION: &str = "import pandas as pd\n\n\ndef run():\n    df = pd.read_json(\"x.json\")\n    return df[\"a\"] + df[\"b\"]\n";

const PRIVATE_SITE: &str = "import pandas as pd\nfrom acme_internal import secret_loader\n\n\ndef run(customer_id):\n    orders = pd.read_csv(\"s3://acme/orders.csv\", sep=\";\")\n    total = orders[\"gross_margin\"].sum()\n    return clean_the_data(orders), total, customer_id\n";

struct Output {
    stdout: String,
    stderr: String,
    code: i32,
}

struct Project {
    dir: TempDir,
    cache: TempDir,
}

impl Project {
    fn new() -> Self {
        let project = Self {
            dir: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
        };
        project.write("pyproject.toml", "");
        project
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let file = self.path().join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, text).unwrap();
        file
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path().join(relative)).unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_input(args, "")
    }

    fn run_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(self.path())
            .env("TYPEDFRAMES_CACHE_DIR", self.cache.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        Output {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            code: output.status.code().unwrap_or(-1),
        }
    }
}
