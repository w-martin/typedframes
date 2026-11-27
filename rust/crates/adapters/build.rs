use std::fs;
use std::path::{Path, PathBuf};

// Exposes a content hash of every crate's sources and the dependency pins as
// `TYPEDFRAMES_BUILD_ID`. The index cache keys on it, so a build with different
// indexing logic, wherever in the workspace it lives, never reuses entries an earlier
// build computed.
fn main() {
    let workspace = Path::new("../..");
    let mut files = vec![workspace.join("Cargo.toml"), workspace.join("Cargo.lock")];
    let crates = workspace.join("crates");
    let mut members: Vec<PathBuf> = fs::read_dir(&crates)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    members.sort();
    for member in &members {
        files.push(member.join("Cargo.toml"));
        collect(&member.join("src"), &mut files);
        println!("cargo:rerun-if-changed={}", member.join("src").display());
    }
    files.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(path).unwrap_or_default();
        let relative = path.strip_prefix(workspace).unwrap_or(path);
        for byte in relative.to_string_lossy().bytes().chain(bytes) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=TYPEDFRAMES_BUILD_ID={hash:016x}");
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}
