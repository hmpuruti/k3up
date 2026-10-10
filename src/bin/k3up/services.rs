use crate::output::Outcome;
use anyhow::{Result, bail};
use k3up::protocol::Response;
use std::path::{Path, PathBuf};

/// Services mode lives in the machine data directory unless a test or an administrator
/// names another one.
pub fn data_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit.or_else(|| std::env::var_os("K3UP_DATA_DIR").map(Into::into)) {
        return Ok(path);
    }
    #[cfg(windows)]
    {
        Ok(k3up::platform::machine_data_dir())
    }
    #[cfg(not(windows))]
    {
        bail!("Services mode is available on Windows only")
    }
}

pub fn enable(data: &Path) -> Result<Response> {
    #[cfg(windows)]
    {
        let lines = k3up::services::setup::enable(data)?;
        Ok(Response::success(lines.join("\n")))
    }
    #[cfg(not(windows))]
    {
        let _ = data;
        bail!("Services mode is available on Windows only")
    }
}

pub fn disable(data: &Path) -> Result<Response> {
    #[cfg(windows)]
    {
        Ok(Response::success(k3up::services::setup::disable(data)?))
    }
    #[cfg(not(windows))]
    {
        let _ = data;
        bail!("Services mode is available on Windows only")
    }
}

pub fn status(data: &Path) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let summary = k3up::services::setup::summary(data)?;
        let counts = if summary.counts.is_empty() {
            "none".to_string()
        } else {
            summary
                .counts
                .iter()
                .map(|(state, count)| format!("{count} {state}"))
                .collect::<Vec<_>>()
                .join("  ·  ")
        };
        let text = format!(
            "Services mode  {}\nData           {}\nHost           {}{}\nWorkloads      {counts}",
            if summary.enabled { "on" } else { "off" },
            summary.data.display(),
            summary.host.display(),
            if summary.host_present {
                ""
            } else {
                "  (missing)"
            },
        );
        Ok(Outcome::Custom {
            text,
            json: serde_json::json!({
                "enabled": summary.enabled,
                "data_dir": summary.data,
                "host": summary.host,
                "host_present": summary.host_present,
                "workloads": summary.counts,
            }),
            ok: true,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = data;
        bail!("Services mode is available on Windows only")
    }
}

/// The agent commands have nothing to act on in services mode.
pub fn refuse_agent(data: &Path) -> Result<()> {
    if k3up::services::active(data) {
        bail!(
            "{} is in services mode: each workload is its own Windows service and there is no agent. See `k3up services status`",
            data.display()
        );
    }
    Ok(())
}
