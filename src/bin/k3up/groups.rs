use crate::{
    output::{Outcome, checked},
    workloads,
};
use anyhow::{Result, bail};
use k3up::{
    client::Client,
    group,
    model::{Manifest, Status},
    protocol::{Command, Response},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Start,
    Stop,
    Restart,
}

impl Verb {
    fn past(self) -> &'static str {
        match self {
            Self::Start => "Started",
            Self::Stop => "Stopped",
            Self::Restart => "Restarted",
        }
    }
}

pub fn check_folder(folder: &str) -> Result<()> {
    if folder.trim().is_empty() {
        bail!("Give a folder path, for example watchtower/entra");
    }
    group::validate(folder)
}

/// The workloads in a folder and its subfolders. An unknown folder is an error.
pub fn members(statuses: Vec<Status>, folder: &str) -> Result<Vec<Status>> {
    check_folder(folder)?;
    let inside: Vec<Status> = statuses
        .into_iter()
        .filter(|status| group::contains(folder, &status.workload.group))
        .collect();
    if inside.is_empty() {
        bail!("No workloads in folder '{folder}'");
    }
    Ok(inside)
}

pub fn list(client: &Client, folder: Option<&str>) -> Result<Response> {
    let mut response = checked(client.send(Command::List)?)?;
    if let Some(folder) = folder {
        response.workloads = members(response.workloads, folder)?;
    }
    Ok(response)
}

/// Members of a folder in startup order.
fn ordered(client: &Client, folder: &str) -> Result<Vec<Status>> {
    let statuses = checked(client.send(Command::List)?)?.workloads;
    let order = Manifest {
        version: 1,
        workloads: statuses
            .iter()
            .map(|status| status.workload.clone())
            .collect(),
    }
    .order()?;
    let mut inside = members(statuses, folder)?;
    inside.sort_by_key(|status| order.iter().position(|name| *name == status.workload.name));
    Ok(inside)
}

pub fn act(client: &Client, folder: &str, verb: Verb, wait: bool, timeout: u64) -> Result<Outcome> {
    let mut targets = ordered(client, folder)?;
    if verb == Verb::Stop {
        targets.reverse();
    }
    let deadline = workloads::deadline(wait, timeout);
    let mut lines = vec![];
    let mut failed = 0;
    for status in &targets {
        let name = &status.workload.name;
        let result = match verb {
            Verb::Start => workloads::start_by(client, name, deadline),
            Verb::Restart => workloads::restart_by(client, name, deadline),
            Verb::Stop => client.send(Command::Stop { name: name.clone() }),
        };
        let problem = match result {
            Ok(response) if response.ok => None,
            Ok(response) => Some(response.message),
            Err(error) => Some(format!("{error:#}")),
        };
        lines.push(match problem {
            None => format!("{} {name}", verb.past()),
            Some(problem) => {
                failed += 1;
                format!("Failed {name}: {problem}")
            }
        });
    }
    let done = targets.len() - failed;
    let mut summary = format!("{done} {}", verb.past().to_lowercase());
    if failed > 0 {
        summary += &format!(", {failed} failed");
    }
    lines.push(summary);
    let statuses = checked(client.send(Command::List)?)?.workloads;
    let response = Response {
        ok: failed == 0,
        message: lines.join("\n"),
        workloads: statuses
            .into_iter()
            .filter(|status| group::contains(folder, &status.workload.group))
            .collect(),
        ..Default::default()
    };
    Ok(Outcome::Custom {
        text: response.message.clone(),
        json: serde_json::to_value(&response)?,
        ok: response.ok,
    })
}

pub fn groups(client: &Client) -> Result<Outcome> {
    let statuses = checked(client.send(Command::List)?)?.workloads;
    let tree = group::tree(&statuses);
    let json = tree
        .folders
        .iter()
        .map(|folder| {
            serde_json::json!({
                "path": folder.path,
                "workloads": folder.workloads(),
                "running": folder.running(),
                "attention": folder.attention(),
            })
        })
        .collect();
    Ok(Outcome::Custom {
        text: tree_text(&tree, &statuses),
        json: serde_json::Value::Array(json),
        ok: true,
    })
}

fn tree_text(tree: &group::Tree, statuses: &[Status]) -> String {
    if statuses.is_empty() {
        return "No workloads".into();
    }
    let mut lines = vec![format!("{:<32} {:>9}  STATE", "FOLDER", "WORKLOADS")];
    for folder in &tree.folders {
        lines.push(format!(
            "{:<32} {:>9}  {}",
            format!("{}{}", "  ".repeat(folder.depth), folder.name()),
            folder.workloads(),
            folder.summary()
        ));
    }
    if !tree.ungrouped.is_empty() {
        let mut loose = group::Folder {
            path: String::new(),
            depth: 0,
            members: tree.ungrouped.clone(),
            counts: Default::default(),
        };
        for &index in &tree.ungrouped {
            *loose.counts.entry(statuses[index].state).or_default() += 1;
        }
        lines.push(format!(
            "{:<32} {:>9}  {}",
            "(no folder)",
            loose.workloads(),
            loose.summary()
        ));
    }
    lines.join("\n")
}
