use anyhow::Context;
use anyhow::Result;
use _rust_linter::{find_project_root, is_enabled, Linter};
use std::env;
use std::fs;
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <file>", args[0]);
        return Ok(());
    }

    let path = Path::new(&args[1]);
    let project_root = find_project_root(path);

    if !is_enabled(&project_root) {
        println!("[]");
        return Ok(());
    }

    let source =
        fs::read_to_string(path).with_context(|| format!("Failed to read file: {:?}", path))?;
    let mut linter = Linter::new();
    let errors = linter.check_file_internal(&source, path)?;

    println!("{}", serde_json::to_string(&errors)?);

    Ok(())
}
