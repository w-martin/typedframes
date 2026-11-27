//! The file system the program reads projects from.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use typedframes_domain::ports::{DirEntry, FileMeta, FileSystem};

/// The machine's own file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsFiles;

impl FileSystem for OsFiles {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        Ok(std::fs::read_dir(path)?
            .flatten()
            .map(|entry| {
                let path = entry.path();
                let is_dir = match entry.file_type() {
                    Ok(kind) if !kind.is_symlink() => kind.is_dir(),
                    _ => path.is_dir(),
                };
                DirEntry { path, is_dir }
            })
            .collect())
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        let metadata = std::fs::metadata(path)?;
        Ok(FileMeta {
            is_file: metadata.is_file(),
            len: metadata.len(),
        })
    }
    fn current_dir(&self) -> Option<PathBuf> {
        std::env::current_dir().ok()
    }
    fn handle(&self) -> Arc<dyn FileSystem> {
        Arc::new(*self)
    }
}
