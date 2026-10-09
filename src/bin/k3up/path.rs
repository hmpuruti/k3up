use anyhow::Result;
use k3up::protocol::Response;
use std::path::PathBuf;

pub fn add(dir: PathBuf, system: bool) -> Result<Response> {
    #[cfg(windows)]
    {
        let (scope, name) = scope(system);
        Ok(Response::success(if k3up::user_path::add(&dir, scope)? {
            format!("Added {} to the {name} PATH", dir.display())
        } else {
            format!("{} is already on the {name} PATH", dir.display())
        }))
    }
    #[cfg(not(windows))]
    {
        let _ = (dir, system);
        anyhow::bail!("Windows only")
    }
}

pub fn remove(dir: PathBuf, system: bool) -> Result<Response> {
    #[cfg(windows)]
    {
        let (scope, name) = scope(system);
        Ok(Response::success(
            if k3up::user_path::remove(&dir, scope)? {
                format!("Removed {} from the {name} PATH", dir.display())
            } else {
                format!("{} is not on the {name} PATH", dir.display())
            },
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = (dir, system);
        anyhow::bail!("Windows only")
    }
}

#[cfg(windows)]
fn scope(system: bool) -> (k3up::user_path::Scope, &'static str) {
    if system {
        (k3up::user_path::Scope::System, "system")
    } else {
        (k3up::user_path::Scope::User, "user")
    }
}
