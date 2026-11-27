use super::*;
use crate::config::LinterConfig;
use crate::ports::test_files::{DirCache, OsFiles};
use std::fs;
use std::path::Path;

mod benches;
mod caching;
mod call_site_resolution;
mod call_sites;
mod excludes;
mod externals;
mod single_file;

// pipeline.py -> loaders.py -> schemas.py: the schema the checked file's diagnostics
// depend on is TWO import hops away, in a file pipeline.py never names itself. Shared
// by the single-file tests below, which differ only in what they then assert.
pub(super) fn write_two_hop_project(root: &Path) -> PathBuf {
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    fs::write(
        root.join("schemas.py"),
        r#"
from typedframes import BaseSchema, Column


class UserSchema(BaseSchema):
    user_id = Column(type=int)
    name = Column(type=str)
"#,
    )
    .unwrap();
    fs::write(
        root.join("loaders.py"),
        r#"
from typing import Annotated

import pandas as pd

from schemas import UserSchema


def load_users(path: str) -> Annotated[pd.DataFrame, UserSchema]:
    return pd.read_csv(path)
"#,
    )
    .unwrap();
    let pipeline_path = root.join("pipeline.py");
    fs::write(
        &pipeline_path,
        r#"
from loaders import load_users


def run(path: str) -> None:
    df = load_users(path)
    print(df["name"])
"#,
    )
    .unwrap();
    pipeline_path
}

pub(super) fn write_import_heavy_project(root: &Path, num_files: usize) {
    fs::write(root.join("pyproject.toml"), "[tool.typedframes]\n").unwrap();
    for hub in 0..5 {
        fs::write(
            root.join(format!("hub{hub}.py")),
            format!(
                r#"
import pandas as pd

def load_hub{hub}(path: str) -> pd.DataFrame:
    return pd.read_csv(path, usecols=["a", "b", "c"])
"#
            ),
        )
        .unwrap();
    }
    for i in 0..num_files {
        let hub = i % 5;
        fs::write(
            root.join(format!("mod_{i}.py")),
            format!(
                r#"
from hub{hub} import load_hub{hub}

def process_{i}(path: str) -> None:
    df = load_hub{hub}(path)
    print(df["a"])
"#
            ),
        )
        .unwrap();
    }
}

pub(super) const LOADER: &str = "import pandas as pd\n\n\ndef load(path):\n    df = pd.read_csv(path, usecols=[\"a\", \"b\"])\n    return df\n";

pub(super) fn project_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pyproject.toml"), "").unwrap();
    for (name, source) in files {
        fs::write(dir.path().join(name), source).unwrap();
    }
    dir
}

pub(super) fn as_json(index: &ProjectIndex) -> serde_json::Value {
    serde_json::to_value(index).unwrap()
}

pub(super) fn call_sites_of(source: &str) -> CallSites {
    let module = ruff_python_parser::parse_module(source)
        .unwrap()
        .into_syntax();
    module_call_sites(&module)
}
