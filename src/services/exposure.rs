//! Services mode runs every workload as LocalSystem, so a program, script or working folder
//! that other users can change lets them run their own code as SYSTEM at the next start.
use crate::model::Workload;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    Program,
    Argument,
    Folder,
    /// A folder on the PATH the process searches for commands it runs by bare name.
    SearchPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposure {
    pub path: PathBuf,
    pub what: Use,
}

/// Arguments that name a file or folder, either whole or as the value of an option such as
/// `--config=C:\app\config.ps1`: any that exists, and any that clearly looks like a path even
/// though it doesn't exist yet, since someone could create it before the next start. A
/// relative path is resolved against `folder`, as Windows resolves it for the process. One that
/// resolves to `folder` or a folder above it is left out, since those are inspected anyway.
pub fn path_arguments(
    args: &[String],
    folder: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    args.iter()
        .filter_map(|arg| {
            let value = arg.split_once('=').map(|(_, value)| value);
            let candidates: Vec<(PathBuf, bool)> = [Some(arg.as_str()), value]
                .into_iter()
                .flatten()
                .filter_map(|text| {
                    Some((resolved(Path::new(text), folder)?, looks_like_path(text)))
                })
                .collect();
            candidates
                .iter()
                .find(|(path, _)| exists(path))
                .or_else(|| candidates.iter().find(|(_, path_like)| *path_like))
                .map(|(path, _)| path.clone())
        })
        .collect()
}

/// Extensions of files that run or load code.
const CODE: [&str; 16] = [
    "bat", "cmd", "com", "dll", "exe", "jar", "js", "pl", "ps1", "psm1", "py", "pyw", "rb", "sh",
    "vbs", "wsf",
];

/// Whether `text` names a path even if nothing is there yet: an absolute path, one with a
/// folder separator, or a file name with a code extension. Switches such as `-File` and `/c`
/// and URLs are not paths.
fn looks_like_path(text: &str) -> bool {
    if text.contains("://") {
        return false;
    }
    if Path::new(text).is_absolute() {
        return true;
    }
    if text.is_empty() || text.starts_with(['-', '/']) {
        return false;
    }
    text.contains(['\\', '/'])
        || Path::new(text)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| CODE.contains(&extension.to_ascii_lowercase().as_str()))
}

fn resolved(path: &Path, folder: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    if !folder.is_absolute() {
        return None;
    }
    let joined = normalised(&folder.join(path));
    (joined.is_absolute() && !normalised(folder).starts_with(&joined)).then_some(joined)
}

/// Removes `.` and resolves `..` without touching the disk, as Windows does with a path
/// before it opens it.
fn normalised(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

/// The folders the process searches for a command given by bare name: the workload's own
/// PATH when it sets one, otherwise `machine_path`, which LocalSystem inherits. Relative
/// entries resolve against the working folder, as the search does.
pub fn search_folders(workload: &Workload, machine_path: Option<String>) -> Vec<PathBuf> {
    let own = workload
        .environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.clone());
    let folder = Path::new(&workload.working_directory);
    own.or(machine_path)
        .unwrap_or_default()
        .split(';')
        .map(|entry| entry.trim().trim_matches('"'))
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let path = Path::new(entry);
            if path.is_absolute() {
                Some(normalised(path))
            } else if folder.is_absolute() {
                Some(normalised(&folder.join(path)))
            } else {
                None
            }
        })
        .collect()
}

/// Every path the workload's process runs or may load code from, once each. `machine_path`
/// is the PATH LocalSystem inherits when the workload sets none.
pub fn inspected(
    workload: &Workload,
    exists: impl Fn(&Path) -> bool,
    machine_path: Option<String>,
) -> Vec<Exposure> {
    let program = Exposure {
        path: workload.executable.clone().into(),
        what: Use::Program,
    };
    let arguments = path_arguments(
        &workload.args,
        Path::new(&workload.working_directory),
        exists,
    )
    .into_iter()
    .map(|path| Exposure {
        path,
        what: Use::Argument,
    });
    let folder = Exposure {
        path: workload.working_directory.clone().into(),
        what: Use::Folder,
    };
    let searched = search_folders(workload, machine_path)
        .into_iter()
        .map(|path| Exposure {
            path,
            what: Use::SearchPath,
        });
    let mut found: Vec<Exposure> = vec![];
    for candidate in std::iter::once(program)
        .chain(arguments)
        .chain(std::iter::once(folder))
        .chain(searched)
    {
        if candidate.path.is_absolute() && found.iter().all(|seen| seen.path != candidate.path) {
            found.push(candidate);
        }
    }
    found
}

/// The paths of `workload` that an account other than SYSTEM, Administrators and
/// TrustedInstaller may change, or move by changing a folder above them, judged through every
/// link along the way. A path whose permissions cannot be read counts as exposed. Always empty
/// outside Windows.
pub fn exposed_paths(workload: &Workload) -> Vec<Exposure> {
    #[cfg(windows)]
    {
        inspected(workload, |path| path.exists(), crate::win32::machine_path())
            .into_iter()
            .filter(|exposure| crate::win32::changeable_by_others(&exposure.path))
            .collect()
    }
    #[cfg(not(windows))]
    {
        let _ = workload;
        vec![]
    }
}

pub fn warning_line(name: &str, path: &Path) -> String {
    format!(
        "Warning: {name} runs as SYSTEM and uses {}, which users who aren't administrators can change. Move it to a folder only administrators can change, such as C:\\Program Files.",
        path.display()
    )
}

pub fn warning_lines(workload: &Workload, exposed: &[Exposure]) -> Vec<String> {
    exposed
        .iter()
        .map(|exposure| warning_line(&workload.name, &exposure.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn arguments_name_existing_absolute_paths_whole_or_after_an_equals_sign() {
        let script = absolute("job.ps1");
        let config = absolute("app.toml");
        let folder = absolute("scripts");
        let existing = [script.clone(), config.clone(), folder.clone()];
        let exists = |path: &Path| existing.iter().any(|known| known == path);
        let args = [
            "-File".to_string(),
            text(&script),
            format!("--config={}", text(&config)),
            text(&folder),
            "relative/job.ps1".into(),
            "--input=relative.txt".into(),
            text(&absolute("missing.ps1")),
            format!("--log={}", text(&absolute("missing.log"))),
            "https://example.com/hook".into(),
            "--url=https://example.com/hook".into(),
        ];
        assert_eq!(
            path_arguments(&args, &absolute("work"), exists),
            [
                script,
                config,
                folder,
                absolute("work").join("relative").join("job.ps1"),
                absolute("missing.ps1"),
                absolute("missing.log"),
            ]
        );
    }

    #[test]
    fn relative_arguments_are_resolved_against_the_working_folder() {
        let root = absolute("k3up-exposure");
        let folder = root.join("work");
        let above = root.join("x").join("job.cmd");
        let bare = folder.join("job.ps1");
        let option = folder.join("sub").join("a.txt");
        let existing = [
            above.clone(),
            bare.clone(),
            option.clone(),
            folder.clone(),
            root.clone(),
        ];
        let exists = |path: &Path| existing.iter().any(|known| known == path);
        let separator = std::path::MAIN_SEPARATOR;
        let args = [
            format!("..{separator}x{separator}job.cmd"),
            "job.ps1".into(),
            format!("--file=sub{separator}a.txt"),
            "missing.ps1".into(),
            format!("--log=logs{separator}missing.log"),
            ".".into(),
            "..".into(),
            format!(".{separator}"),
            "--dir=..".into(),
        ];
        assert_eq!(
            path_arguments(&args, &folder, exists),
            [
                above,
                bare,
                option,
                folder.join("missing.ps1"),
                folder.join("logs").join("missing.log"),
            ]
        );
        assert!(path_arguments(&["job.ps1".into()], Path::new("work"), |_| true).is_empty());
    }

    #[test]
    fn switches_urls_and_plain_words_are_not_paths() {
        for text in [
            "-File",
            "https://example.com/a.ps1",
            "relative.txt",
            "web",
            "",
        ] {
            assert!(!looks_like_path(text), "{text}");
        }
        // On Windows a leading slash starts a switch; elsewhere it starts an absolute path.
        if cfg!(windows) {
            assert!(!looks_like_path("/c") && !looks_like_path("/S"));
        }
        for text in ["scripts\\job.cmd", "scripts/job.cmd", "job.PS1", "tool.exe"] {
            assert!(looks_like_path(text), "{text}");
        }
    }

    #[test]
    fn the_program_arguments_and_folder_are_each_inspected_once() {
        let program = absolute("tool.exe");
        let folder = absolute("work");
        let script = absolute("run.cmd");
        let workload = Workload {
            name: "web".into(),
            executable: text(&program),
            working_directory: text(&folder),
            args: vec![text(&script), text(&program), text(&folder)],
            ..Default::default()
        };
        let found = inspected(&workload, |_| true, None);
        let paths: Vec<_> = found
            .iter()
            .map(|exposure| (exposure.path.clone(), exposure.what))
            .collect();
        assert_eq!(
            paths,
            [
                (program, Use::Program),
                (script.clone(), Use::Argument),
                (folder, Use::Argument),
            ]
        );
        let lines = warning_lines(&workload, &found[1..2]);
        assert_eq!(
            lines,
            [format!(
                "Warning: web runs as SYSTEM and uses {}, which users who aren't administrators can change. Move it to a folder only administrators can change, such as C:\\Program Files.",
                script.display()
            )]
        );
    }

    #[test]
    fn search_folders_come_from_the_workloads_path_or_the_machines() {
        let folder = absolute("work");
        let bin = absolute("bin");
        let tools = absolute("tools");
        let separator = std::path::MAIN_SEPARATOR;
        let mut workload = Workload {
            name: "web".into(),
            working_directory: text(&folder),
            ..Default::default()
        };
        let machine = Some(format!("{};;\"{}\"", text(&bin), text(&tools)));
        assert_eq!(
            search_folders(&workload, machine.clone()),
            [bin.clone(), tools.clone()]
        );

        workload.environment.insert(
            "Path".into(),
            format!("{};local{separator}bin", text(&tools)),
        );
        assert_eq!(
            search_folders(&workload, machine),
            [tools, folder.join("local").join("bin")]
        );
        let found = inspected(&workload, |_| false, None);
        assert!(found.iter().any(|exposure| exposure.what == Use::SearchPath
            && exposure.path == folder.join("local").join("bin")));
    }

    #[test]
    fn a_definition_the_caller_cannot_read_has_nothing_to_inspect() {
        let hidden = Workload {
            name: "web".into(),
            ..Default::default()
        };
        assert!(inspected(&hidden, |_| true, None).is_empty());
        assert!(exposed_paths(&hidden).is_empty());
    }
}
