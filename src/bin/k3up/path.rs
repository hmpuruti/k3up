use anyhow::Result;
use k3up::protocol::Response;
use std::path::PathBuf;

pub fn add(dir: PathBuf) -> Result<Response> {
    #[cfg(windows)]
    {
        Ok(Response::success(if k3up::user_path::add(&dir)? {
            format!("Added {} to the user PATH", dir.display())
        } else {
            format!("{} is already on the user PATH", dir.display())
        }))
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        anyhow::bail!("Windows only")
    }
}

pub fn remove(dir: PathBuf) -> Result<Response> {
    #[cfg(windows)]
    {
        Ok(Response::success(if k3up::user_path::remove(&dir)? {
            format!("Removed {} from the user PATH", dir.display())
        } else {
            format!("{} is not on the user PATH", dir.display())
        }))
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        anyhow::bail!("Windows only")
    }
}
