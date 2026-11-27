//! What the analysis needs from outside itself. The analysis follows imports into other files and
//! reads `pyproject.toml` and `.sql` files, but never touches the file system directly: it asks a
//! [`FileSystem`], which the application plugs in. Applying fixes writes through a [`Workspace`].

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One entry of a directory listing.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    /// Whether it is a directory, following a symbolic link to see what it points at.
    pub is_dir: bool,
}

/// What a path holds: whether it is a regular file, and its size.
#[derive(Debug, Clone, Copy)]
pub struct FileMeta {
    pub is_file: bool,
    pub len: u64,
}

/// Read access to the files of the project being checked, and the files it imports.
pub trait FileSystem: Send + Sync {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn is_file(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn exists(&self, path: &Path) -> bool;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    fn metadata(&self, path: &Path) -> io::Result<FileMeta>;
    /// The directory relative paths are relative to.
    fn current_dir(&self) -> Option<PathBuf>;
    /// This file system as a shared handle, for something that keeps it.
    fn handle(&self) -> Arc<dyn FileSystem>;
}

/// Where indexing results are kept between runs, by name.
pub trait IndexCache: Send + Sync {
    /// What was last stored under `name`, if anything.
    fn load(&self, name: &str) -> Option<Vec<u8>>;
    /// Keeps `bytes` under `name` in place of what was there. Best effort: a cache that can't
    /// be written is only slower next time.
    fn store(&self, name: &str, bytes: &[u8]);
    /// The build that reads and writes these entries; a different build never reuses them,
    /// as its indexing may differ without a version bump.
    fn build_id(&self) -> &str;
}

/// The working copy that `apply` edits.
pub trait Workspace: Send + Sync {
    /// Whether version control tracks `path`, so an edit to it can be undone.
    fn is_tracked(&self, path: &Path) -> bool;
    /// Replaces the content of `path` with `content`, never leaving it half-written.
    fn write_atomically(&self, path: &Path, content: &str) -> io::Result<()>;
}

/// A shared [`FileSystem`], empty ([`NoFiles`]) unless one is given.
#[derive(Clone)]
pub struct FileSystemHandle(Arc<dyn FileSystem>);

impl Default for FileSystemHandle {
    fn default() -> Self {
        FileSystemHandle(Arc::new(NoFiles))
    }
}

impl std::ops::Deref for FileSystemHandle {
    type Target = dyn FileSystem;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl From<Arc<dyn FileSystem>> for FileSystemHandle {
    fn from(fs: Arc<dyn FileSystem>) -> Self {
        FileSystemHandle(fs)
    }
}

impl<T: FileSystem + 'static> From<Arc<T>> for FileSystemHandle {
    fn from(fs: Arc<T>) -> Self {
        FileSystemHandle(fs)
    }
}

/// A file system with nothing in it: what a linter checking source it was handed, with no
/// project around it, sees.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFiles;

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no file system")
}

impl FileSystem for NoFiles {
    fn read_to_string(&self, _path: &Path) -> io::Result<String> {
        Err(not_found())
    }
    fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
        Err(not_found())
    }
    fn is_file(&self, _path: &Path) -> bool {
        false
    }
    fn is_dir(&self, _path: &Path) -> bool {
        false
    }
    fn exists(&self, _path: &Path) -> bool {
        false
    }
    fn canonicalize(&self, _path: &Path) -> io::Result<PathBuf> {
        Err(not_found())
    }
    fn read_dir(&self, _path: &Path) -> io::Result<Vec<DirEntry>> {
        Err(not_found())
    }
    fn metadata(&self, _path: &Path) -> io::Result<FileMeta> {
        Err(not_found())
    }
    fn current_dir(&self) -> Option<PathBuf> {
        None
    }
    fn handle(&self) -> Arc<dyn FileSystem> {
        Arc::new(*self)
    }
}

/// The real file system, for the unit tests of this crate; `typedframes_adapters` has the one
/// the program uses.
#[cfg(test)]
pub(crate) mod test_files {
    use super::{DirEntry, FileMeta, FileSystem, IndexCache};
    use std::io;
    use std::path::{Path, PathBuf};

    #[derive(Debug, Default, Clone, Copy)]
    pub(crate) struct OsFiles;

    /// A cache kept as files in a directory.
    pub(crate) struct DirCache(pub(crate) PathBuf);

    impl IndexCache for DirCache {
        fn load(&self, name: &str) -> Option<Vec<u8>> {
            std::fs::read(self.0.join(name)).ok()
        }
        fn store(&self, name: &str, bytes: &[u8]) {
            let _ = std::fs::create_dir_all(&self.0);
            let _ = std::fs::write(self.0.join(name), bytes);
        }
        fn build_id(&self) -> &str {
            "test"
        }
    }

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
        fn handle(&self) -> std::sync::Arc<dyn FileSystem> {
            std::sync::Arc::new(*self)
        }
    }
}
