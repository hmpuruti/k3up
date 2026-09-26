use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The absolute path of a program given as a path or as a bare name found on PATH.
/// Symlinks are kept as given, so release links and multi-call binaries keep working.
pub fn executable(program: &str) -> Result<PathBuf> {
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let path = if program.contains(separators) {
        std::path::absolute(program)?
    } else {
        search(program).with_context(|| format!("Program '{program}' was not found on PATH"))?
    };
    if !path.is_file() {
        bail!("Executable {} does not exist", path.display());
    }
    Ok(path)
}

pub fn directory(dir: &Path) -> Result<PathBuf> {
    let directory = std::path::absolute(dir)?;
    if !directory.is_dir() {
        bail!("Working directory {} does not exist", directory.display());
    }
    Ok(directory)
}

fn search(name: &str) -> Option<PathBuf> {
    search_in(name, &std::env::var_os("PATH")?)
}

fn search_in(name: &str, paths: &std::ffi::OsStr) -> Option<PathBuf> {
    let candidates = candidates(name);
    std::env::split_paths(paths)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates.iter().map(move |file| dir.join(file)))
        .find(|path| runnable(path))
        .and_then(|path| std::path::absolute(path).ok())
}

/// On Windows a bare name usually omits its extension, so every PATHEXT extension is tried.
fn candidates(name: &str) -> Vec<String> {
    let mut found = vec![name.to_string()];
    if cfg!(windows) {
        let extensions =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        found.extend(
            extensions
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(|extension| format!("{name}{}", extension.to_lowercase())),
        );
    }
    found
}

fn runnable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_names_resolve_through_path() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir
            .path()
            .join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::write(&program, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let paths = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(
            search_in("tool", &paths).unwrap(),
            std::path::absolute(&program).unwrap()
        );
        assert_eq!(search_in("no-such-program", &paths), None);
        assert!(
            executable("no-such-program-k3up")
                .unwrap_err()
                .to_string()
                .contains("not found on PATH")
        );
    }

    #[test]
    fn paths_are_made_absolute_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("run");
        std::fs::write(&program, b"").unwrap();
        assert_eq!(
            executable(program.to_str().unwrap()).unwrap(),
            std::path::absolute(&program).unwrap()
        );
        assert!(executable("./does-not-exist").is_err());
    }
}
