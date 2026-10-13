//! Path handling that matches what Python's `Path` did for the command line.

use std::env;
use std::path::{Component, Path, PathBuf};

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `Path.expanduser()`: a leading `~` or `~/` becomes the home directory.
pub(crate) fn expanduser(path: &Path) -> PathBuf {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "~" => match home_dir() {
            Some(home) => home.join(components.as_path()),
            None => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

/// `Path.resolve()`: absolute, `.` and `..` collapsed, symlinks followed as far as the path exists,
/// whether or not it does. Done component by component, as `realpath` does, so a `..` after a
/// symlink climbs out of the link's target.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::Normal(name) => {
                resolved.push(name);
                if let Ok(real) = resolved.canonicalize() {
                    resolved = strip_verbatim(real);
                }
            }
        }
    }
    resolved
}

/// `repr(path)` for a `Path`: `PosixPath('x')` or `WindowsPath('x')`.
pub(crate) fn repr(path: &Path) -> String {
    let kind = if cfg!(windows) {
        "WindowsPath"
    } else {
        "PosixPath"
    };
    format!("{kind}({})", super::pyfmt::repr_str(&posix(path)))
}

/// `path` relative to `root` in POSIX form, or `path` in POSIX form when it is not under `root`.
pub(crate) fn relative_posix(path: &str, root: &Path) -> String {
    let path = Path::new(path);
    let relative = path.strip_prefix(root).unwrap_or(path);
    if relative.as_os_str().is_empty() {
        return ".".to_string();
    }
    posix(relative)
}

/// `Path.as_posix()`.
pub(crate) fn posix(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_resolve_a_missing_path_below_an_existing_directory() {
        // arrange
        let dir = tempfile::tempdir().unwrap();
        let base = resolve(dir.path());

        // act
        let resolved = resolve(&dir.path().join("a").join("..").join("b").join("c.py"));

        // assert
        assert_eq!(base.join("b").join("c.py"), resolved);
    }

    #[test]
    fn test_should_expand_a_leading_tilde_only() {
        // arrange
        let plain = Path::new("a/~b");

        // act
        let expanded = expanduser(plain);

        // assert
        assert_eq!(plain, expanded);
    }

    #[test]
    fn test_should_show_a_path_the_way_pathlib_does() {
        assert_eq!("PosixPath('src/x.py')", repr(Path::new("src/x.py")));
    }

    #[test]
    fn test_should_keep_a_path_outside_the_root_whole() {
        assert_eq!(
            "/other/x.py",
            relative_posix("/other/x.py", Path::new("/proj"))
        );
        assert_eq!(
            "src/x.py",
            relative_posix("/proj/src/x.py", Path::new("/proj"))
        );
    }
}
