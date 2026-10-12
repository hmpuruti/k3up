//! Services mode runs every workload as LocalSystem, so a program, script or working folder
//! that other users can change lets them run their own code as SYSTEM at the next start.
use crate::model::Workload;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    Program,
    Argument,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposure {
    pub path: PathBuf,
    pub what: Use,
}

/// Arguments that name an existing file or folder, either whole or as the value of an option
/// such as `--config=C:\app\config.ps1`. A relative path is resolved against `folder`, as
/// Windows resolves it for the process. One that resolves to `folder` or a folder above it is
/// left out, since those are inspected anyway.
pub fn path_arguments(
    args: &[String],
    folder: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    args.iter()
        .filter_map(|arg| {
            let value = arg.split_once('=').map(|(_, value)| value);
            [Some(arg.as_str()), value]
                .into_iter()
                .flatten()
                .filter_map(|text| resolved(Path::new(text), folder))
                .find(|path| exists(path))
        })
        .collect()
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

/// Every path the workload's process runs or may load code from, once each.
pub fn inspected(workload: &Workload, exists: impl Fn(&Path) -> bool) -> Vec<Exposure> {
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
    let mut found: Vec<Exposure> = vec![];
    for candidate in std::iter::once(program)
        .chain(arguments)
        .chain(std::iter::once(folder))
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
        inspected(workload, |path| path.exists())
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
            [script, config, folder]
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
            [above, bare, option]
        );
        assert!(path_arguments(&["job.ps1".into()], Path::new("work"), |_| true).is_empty());
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
        let found = inspected(&workload, |_| true);
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
    fn a_definition_the_caller_cannot_read_has_nothing_to_inspect() {
        let hidden = Workload {
            name: "web".into(),
            ..Default::default()
        };
        assert!(inspected(&hidden, |_| true).is_empty());
        assert!(exposed_paths(&hidden).is_empty());
    }
}
