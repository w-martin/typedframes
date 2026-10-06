use std::fs;
use std::path::{Path, PathBuf};

// Exposes a content hash of the crate's sources and dependency pins as
// `TYPEDFRAMES_BUILD_ID`. The index cache keys on it, so a build with different
// indexing logic never reuses entries an earlier build computed.
fn main() {
    let mut files = vec![PathBuf::from("Cargo.toml"), PathBuf::from("Cargo.lock")];
    collect(Path::new("src"), &mut files);
    files.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(path).unwrap_or_default();
        for byte in path.to_string_lossy().bytes().chain(bytes) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    println!("cargo:rerun-if-changed=src");
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
