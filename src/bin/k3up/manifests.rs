use crate::output::checked;
use anyhow::{Context, Result};
use k3up::{
    client::Client,
    model::Manifest,
    protocol::{Command, Response},
};
use std::path::{Path, PathBuf};

pub fn read(file: &Path) -> Result<Manifest> {
    toml::from_str(
        &std::fs::read_to_string(file).with_context(|| format!("Read {}", file.display()))?,
    )
    .context("Parse TOML manifest")
}

pub fn validate(file: &Path) -> Result<Response> {
    let manifest = read(file)?;
    let order = manifest.validate()?;
    let now = chrono::Utc::now();
    for workload in &manifest.workloads {
        workload.first_run(now)?;
    }
    Ok(Response::success(format!(
        "Valid. Startup order: {}",
        order.join(" -> ")
    )))
}

pub fn apply(client: &Client, file: &Path, dry_run: bool) -> Result<Response> {
    client.send(Command::Apply {
        manifest: read(file)?,
        dry_run,
    })
}

pub fn export(client: &Client, output: Option<PathBuf>) -> Result<Response> {
    let response = checked(client.send(Command::Export)?)?;
    let Some(output) = output else {
        return Ok(response);
    };
    std::fs::write(&output, response.manifest.as_ref().unwrap().to_toml()?)
        .with_context(|| format!("Write {}", output.display()))?;
    Ok(Response::success(format!("Saved {}", output.display())))
}

pub fn template() -> Response {
    Response {
        text: Some(crate::template::TEMPLATE.trim_end().to_string()),
        ..Response::success("Template")
    }
}
