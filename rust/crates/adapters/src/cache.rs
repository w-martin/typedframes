//! The index cache kept on disk, one file per project, readable by the user alone.

use std::fs;
use std::path::{Path, PathBuf};
use typedframes_domain::ports::IndexCache;

/// A cache directory on disk.
#[derive(Debug, Clone)]
pub struct DiskIndexCache {
    dir: PathBuf,
}

impl DiskIndexCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        DiskIndexCache { dir: dir.into() }
    }
}

impl IndexCache for DiskIndexCache {
    fn load(&self, name: &str) -> Option<Vec<u8>> {
        fs::read(self.dir.join(name)).ok()
    }

    fn store(&self, name: &str, bytes: &[u8]) {
        let cache_path = self.dir.join(name);
        let Some(dir) = cache_path.parent() else {
            return;
        };
        if create_private_dir(dir).is_err() {
            return;
        }
        // Renamed into place so a reader or a crash never sees a half-written file.
        let temporary = cache_path.with_extension(format!("tmp{}", std::process::id()));
        if write_private(&temporary, bytes).is_err() || fs::rename(&temporary, &cache_path).is_err()
        {
            let _ = fs::remove_file(&temporary);
        }
    }

    fn build_id(&self) -> &str {
        env!("TYPEDFRAMES_BUILD_ID")
    }
}

// Cache files hold source paths and identifiers, so only the user may read them.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

// A directory made for the cache is private to the user; an existing one is left alone.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_read_back_what_was_stored() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskIndexCache::new(dir.path().join("typedframes"));

        cache.store("index-a.bin", b"bytes");

        assert_eq!(cache.load("index-a.bin").as_deref(), Some(&b"bytes"[..]));
        assert_eq!(cache.load("index-b.bin"), None);
    }

    #[test]
    fn test_should_leave_no_temporary_file_behind_when_the_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskIndexCache::new(dir.path());
        fs::create_dir(dir.path().join("index-a.bin")).unwrap();

        cache.store("index-a.bin", b"bytes");

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("index-a.bin")]);
    }

    #[cfg(unix)]
    #[test]
    fn test_should_create_the_cache_directory_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("typedframes");

        DiskIndexCache::new(&dir).store("index-a.bin", b"bytes");

        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn test_should_write_cache_files_readable_by_the_user_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();

        DiskIndexCache::new(dir.path()).store("index-a.bin", b"bytes");

        let mode = fs::metadata(dir.path().join("index-a.bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn test_should_survive_a_cache_dir_that_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "a file, not a directory").unwrap();
        let cache = DiskIndexCache::new(blocker.join("cache"));

        cache.store("index-a.bin", b"bytes");

        assert_eq!(cache.load("index-a.bin"), None);
    }
}
