use crate::manifests;
use anyhow::Result;
use k3up::{protocol::Response, systemd};
use std::path::Path;

pub fn export(file: &Path, output: &Path) -> Result<Response> {
    let count = systemd::export(&manifests::read(file)?, output)?;
    Ok(Response::success(format!(
        "Exported {count} native units to {}",
        output.display()
    )))
}

#[cfg(not(target_os = "linux"))]
pub fn install(file: &Path) -> Result<Response> {
    let _ = file;
    anyhow::bail!("systemd-install requires Linux. Use systemd-export to generate units on this OS")
}

#[cfg(target_os = "linux")]
pub fn install(file: &Path) -> Result<Response> {
    use anyhow::Context;
    use std::path::PathBuf;
    let manifest = manifests::read(file)?;
    let directory = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
        .join(".config/systemd/user");
    let units = systemd::render(&manifest)?;
    // Only install new units, avoiding silent replacement of administrator changes.
    for name in units.keys() {
        if directory.join(name).exists() {
            anyhow::bail!("{name} already exists; export and review changes first");
        }
    }
    let run = |arguments: &[&str]| -> Result<()> {
        let status = std::process::Command::new("systemctl")
            .arg("--user")
            .args(arguments)
            .status()?;
        anyhow::ensure!(status.success(), "systemctl failed: {arguments:?}");
        Ok(())
    };
    let installed = systemd::export(&manifest, &directory).and_then(|_| {
        run(&["daemon-reload"])?;
        for workload in &manifest.workloads {
            if workload.start_at_boot {
                run(&["enable", &format!("k3up-{}.service", workload.name)])?;
            }
            if workload.schedule.is_some() {
                run(&["enable", "--now", &format!("k3up-{}.timer", workload.name)])?;
            }
        }
        Ok(())
    });
    if let Err(error) = installed {
        // Undo this run only; every file removed here was absent before it started.
        for name in units.keys() {
            let _ = run(&["disable", "--now", name]);
            let _ = std::fs::remove_file(directory.join(name));
        }
        let _ = run(&["daemon-reload"]);
        return Err(error.context("Installation rolled back"));
    }
    Ok(Response::success(
        "Installed user units. Use systemctl --user to manage them; `loginctl enable-linger` is required to run them without a login session",
    ))
}
