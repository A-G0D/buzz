use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Session-scoped utility aliases. Git is configured by the ACP harness.
pub struct Shim {
    _dir: TempDir,
    pub path_env: String,
}

impl Shim {
    pub fn install() -> std::io::Result<Self> {
        let dir = tempfile::Builder::new().prefix("buzz-dev-mcp-").tempdir()?;
        set_owner_only(dir.path())?;
        let self_exe = std::env::current_exe()?;
        for name in ["rg", "tree", "buzz"] {
            symlink(&self_exe, &dir.path().join(name))?;
        }
        let original = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![PathBuf::from(dir.path())];
        entries.extend(std::env::split_paths(&original));
        let path_env = std::env::join_paths(entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            _dir: dir,
            path_env,
        })
    }

    pub fn buzz_cli_path(&self) -> PathBuf {
        #[cfg(windows)]
        let name = "buzz.exe";
        #[cfg(not(windows))]
        let name = "buzz";
        self._dir.path().join(name)
    }

    /// Resolve one executable name against the exact PATH exposed to tools.
    pub fn resolve_executable(&self, name: &str) -> Option<PathBuf> {
        let relative = Path::new(name);
        if name.is_empty()
            || relative.is_absolute()
            || relative.components().count() != 1
            || relative.file_name()?.to_str()? != name
        {
            return None;
        }
        for directory in std::env::split_paths(std::ffi::OsStr::new(&self.path_env)) {
            if directory == self._dir.path() {
                continue;
            }
            let candidate = directory.join(name);
            if let Some(path) = executable_path(&candidate) {
                return Some(path);
            }
            #[cfg(windows)]
            if candidate.extension().is_none() {
                if let Some(path) = executable_path(&candidate.with_extension("exe")) {
                    return Some(path);
                }
            }
        }
        None
    }
}

fn executable_path(path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    let metadata = canonical.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(canonical)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_owner_only(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(not(unix))]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    // No symlinks without elevation on Windows; copy instead. The target needs
    // a .exe extension or PATH lookup (via PATHEXT) won't treat it as runnable.
    let dst = dst.with_extension("exe");
    std::fs::copy(src, dst).map(|_| ())
}

pub fn artifact_dir(session_root: &Path) -> PathBuf {
    let p = session_root.join("artifacts");
    let _ = std::fs::create_dir_all(&p);
    p
}

#[cfg(test)]
mod tests {
    use super::Shim;

    #[test]
    fn exposes_the_session_scoped_buzz_multicall_alias() {
        let shim = Shim::install().unwrap();
        assert!(shim.buzz_cli_path().is_file());
    }

    #[cfg(unix)]
    #[test]
    fn resolves_only_executable_names_from_the_tool_path() {
        use std::os::unix::fs::PermissionsExt as _;

        let mut shim = Shim::install().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("buzz-acp");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        shim.path_env = std::env::join_paths(
            std::iter::once(directory.path().to_path_buf())
                .chain(std::env::split_paths(std::ffi::OsStr::new(&shim.path_env))),
        )
        .unwrap()
        .to_string_lossy()
        .into_owned();

        assert_eq!(
            shim.resolve_executable("buzz-acp"),
            executable.canonicalize().ok()
        );
        assert!(shim.resolve_executable("../buzz-acp").is_none());

        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(shim.resolve_executable("buzz-acp").is_none());
    }
}
