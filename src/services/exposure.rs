//! Services mode runs every workload as LocalSystem, so a program, script or working folder
//! that other users can change lets them run their own code as SYSTEM at the next start.
use crate::model::Workload;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    Program,
    /// The program's own folder, where Windows looks first for the DLLs the program loads.
    ProgramFolder,
    Argument,
    /// An argument still holding a variable K3 Up can't expand, so its path can't be checked.
    Unresolved,
    /// The folder of a script argument, where interpreters such as Python look for modules.
    ScriptFolder,
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
        .flat_map(|arg| {
            let whole = path_in(arg, folder, &exists);
            if whole.as_ref().is_some_and(|path| exists(path)) {
                return whole.into_iter().collect();
            }
            // A command string such as `/c "call ..\job.cmd && next.cmd"` holds several
            // commands, which a shell runs one word at a time.
            let words = words(arg);
            if words.len() < 2 {
                return whole.into_iter().collect();
            }
            words
                .iter()
                .filter_map(|word| path_in(word, folder, &exists))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The path `text` names, whole or after `=`: one that exists, or else one that clearly
/// looks like a path.
fn path_in(text: &str, folder: &Path, exists: &impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let value = text.split_once('=').map(|(_, value)| value);
    let candidates: Vec<(PathBuf, bool)> = [Some(text), value]
        .into_iter()
        .flatten()
        .filter_map(|text| Some((resolved(Path::new(text), folder)?, looks_like_path(text))))
        .collect();
    candidates
        .iter()
        .find(|(path, _)| exists(path))
        .or_else(|| candidates.iter().find(|(_, path_like)| *path_like))
        .map(|(path, _)| path.clone())
}

/// Splits a command string into words as a shell would: at spaces and at the command
/// separators `& | < > ( )`, keeping quoted text together and dropping the quotes.
fn words(text: &str) -> Vec<String> {
    let mut words = vec![];
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match (quote, c) {
            (Some(open), _) if c == open => quote = None,
            (Some(_), _) => word.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() || "&|<>()".contains(c) => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            (None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
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

/// What a LocalSystem process inherits that decides where its paths lead: the machine PATH,
/// and the values of variables such as `%PUBLIC%` that a shell expands in its arguments.
#[derive(Debug, Clone, Default)]
pub struct Inherited {
    pub path: Option<String>,
    pub variables: Vec<(String, String)>,
    /// Reads any other machine-wide variable as stored, such as a custom `%TOOLS%`.
    pub machine: Option<fn(&str) -> Option<String>>,
}

impl Inherited {
    /// A variable as a LocalSystem process starts with it: a known folder, or else the
    /// machine-wide value, expanded once with the known folders as Windows does.
    fn value(&self, name: &str) -> Option<String> {
        let known = |name: &str| {
            self.variables
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        };
        known(name).or_else(|| Some(expand(&(self.machine?)(name)?, known)))
    }
}

impl Inherited {
    /// What services running as LocalSystem on this machine start with, worked out without
    /// trusting the caller's environment.
    #[cfg(windows)]
    pub fn local_system() -> Self {
        let mut inherited = Self {
            path: None,
            variables: crate::win32::local_system_variables(),
            machine: Some(crate::win32::machine_variable),
        };
        inherited.path =
            crate::win32::machine_path().map(|path| expand(&path, |name| inherited.value(name)));
        inherited
    }
}

/// Replaces each `%NAME%`, and then each delayed `!NAME!`, that `lookup` knows, as cmd.exe
/// expands them, ignoring case. Unknown names are left as written.
pub fn expand(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let once = expand_between(text, '%', &lookup);
    expand_between(&once, '!', &lookup)
}

fn expand_between(text: &str, mark: char, lookup: &impl Fn(&str) -> Option<String>) -> String {
    let mut expanded = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(mark) {
        let after = &rest[start + 1..];
        let Some(end) = after.find(mark) else {
            break;
        };
        let name = &after[..end];
        expanded.push_str(&rest[..start]);
        match (!name.is_empty()).then(|| lookup(name)).flatten() {
            Some(value) => {
                expanded.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                expanded.push(mark);
                rest = after;
            }
        }
    }
    expanded.push_str(rest);
    expanded
}

/// Programs that run commands or code given as an argument. Their command strings follow
/// parsing rules K3 Up can't fully reproduce, so such strings are always warned about.
const INTERPRETERS: [&str; 14] = [
    "bash",
    "cmd",
    "cscript",
    "node",
    "perl",
    "php",
    "powershell",
    "pwsh",
    "py",
    "python",
    "python3",
    "ruby",
    "sh",
    "wscript",
];

/// The extensions Windows tries for a command given without one, when PATHEXT isn't set.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC";

/// The files a shell would run for the bare commands in `args`, such as `helper` or
/// `helper.exe`: the first match in the working folder and then in `searched`, trying each
/// PATHEXT extension for a command given without one.
fn commands_found(
    workload: &Workload,
    inherited: &Inherited,
    args: &[String],
    searched: &[PathBuf],
    exists: &impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let extensions: Vec<String> = variable(workload, inherited, "PATHEXT")
        .unwrap_or_else(|| DEFAULT_PATHEXT.into())
        .split(';')
        .map(|extension| extension.trim().to_string())
        .filter(|extension| !extension.is_empty())
        .collect();
    let folders: Vec<PathBuf> = std::iter::once(PathBuf::from(&workload.working_directory))
        .chain(searched.iter().cloned())
        .filter(|folder| folder.is_absolute())
        .collect();
    args.iter()
        .flat_map(|arg| words(arg))
        .filter(|word| !word.starts_with(['-', '/']) && !word.contains(['\\', '/', ':']))
        .filter_map(|word| {
            let names: Vec<String> = if Path::new(&word).extension().is_some() {
                vec![word.clone()]
            } else {
                extensions
                    .iter()
                    .map(|extension| format!("{word}{extension}"))
                    .collect()
            };
            folders.iter().find_map(|folder| {
                names
                    .iter()
                    .map(|name| folder.join(name))
                    .find(|path| exists(path))
                    .map(|path| as_listed(&path))
            })
        })
        .collect()
}

/// `path` with its file name spelled as the folder lists it, since a PATHEXT extension such
/// as `.CMD` matches `helper.cmd` on Windows but should be reported as the file is named.
fn as_listed(path: &Path) -> PathBuf {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    let wanted = name.to_string_lossy().to_lowercase();
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().to_lowercase() == wanted)
        .map(|entry| entry.path())
        .unwrap_or_else(|| path.to_path_buf())
}

/// A file that runs or loads code, judged by its extension.
fn is_code(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| CODE.contains(&extension.to_ascii_lowercase().as_str()))
}

/// The positions of arguments PowerShell decodes before running them, given after
/// `-EncodedCommand`, `-EncodedArguments` or any abbreviation PowerShell accepts. Their
/// content can't be read as paths.
fn encoded_payloads(program: &str, args: &[String]) -> Vec<usize> {
    let powershell = Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| {
            stem.eq_ignore_ascii_case("powershell") || stem.eq_ignore_ascii_case("pwsh")
        });
    if !powershell {
        return vec![];
    }
    args.iter()
        .enumerate()
        .filter(|(_, arg)| {
            let switch = arg.trim_start_matches(['-', '/']).to_ascii_lowercase();
            arg.starts_with(['-', '/'])
                && !switch.is_empty()
                && (switch == "ec"
                    || switch == "ea"
                    || "encodedcommand".starts_with(&switch)
                    || (switch.len() >= 8 && "encodedarguments".starts_with(&switch)))
        })
        .map(|(index, _)| index + 1)
        .filter(|index| *index < args.len())
        .collect()
}

fn runs_commands(program: &str) -> bool {
    Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| INTERPRETERS.contains(&stem.to_ascii_lowercase().as_str()))
}

/// An argument that holds a command rather than a single word: it has spaces or shell
/// symbols. Separate arguments such as `-File C:\x.ps1` are words, not commands.
fn is_command(arg: &str) -> bool {
    arg.chars()
        .any(|c| c.is_whitespace() || "&|<>^();`$".contains(c))
}

/// Whether `text` still refers to a variable once expanded: an unknown `%NAME%` or `!NAME!`, a
/// cmd modifier such as `%NAME:~0,3%`, or PowerShell's `$env:NAME`. Its path can't be known
/// until the process runs.
pub fn unresolved(text: &str) -> bool {
    let between = |mark: char| {
        let mut parts = text.split(mark).skip(1);
        let mut found = false;
        while let (Some(name), Some(_)) = (parts.next(), parts.clone().next()) {
            if !name.is_empty() && !name.contains(char::is_whitespace) {
                found = true;
                break;
            }
        }
        found
    };
    let lower = text.to_ascii_lowercase();
    between('%') || between('!') || lower.contains("$env:") || lower.contains("${env:")
}

/// A variable as the workload's process sees it: its own environment first, then what it
/// inherits.
fn variable(workload: &Workload, inherited: &Inherited, name: &str) -> Option<String> {
    workload
        .environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .or_else(|| inherited.value(name))
}

/// The variables an interpreter reads to find modules or libraries it loads by name, such as
/// Python's `PYTHONPATH`. Their folders matter only when the workload runs that interpreter.
fn module_variables(program: &str) -> &'static [&'static str] {
    let stem = Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match stem.as_str() {
        "python" | "python3" | "pythonw" | "py" => &["PYTHONPATH"],
        "node" => &["NODE_PATH"],
        "perl" => &["PERL5LIB", "PERLLIB"],
        "ruby" => &["RUBYLIB"],
        "powershell" | "pwsh" => &["PSModulePath"],
        _ => &[],
    }
}

/// The folders in a `;`-separated list such as PATH, with relative entries resolved against
/// the working folder.
fn folders_in(list: &str, folder: &Path) -> Vec<PathBuf> {
    list.split(';')
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

/// The folders the process searches for a command given by bare name: the workload's own
/// PATH when it sets one, otherwise `machine_path`, which LocalSystem inherits. Relative
/// entries resolve against the working folder, as the search does.
pub fn search_folders(workload: &Workload, machine_path: Option<String>) -> Vec<PathBuf> {
    let own = workload
        .environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.clone());
    folders_in(
        &own.or(machine_path).unwrap_or_default(),
        Path::new(&workload.working_directory),
    )
}

/// Every path the workload's process runs or may load code from, once each. Arguments are
/// read after expanding `%NAME%` variables, as a shell such as cmd.exe would.
pub fn inspected(
    workload: &Workload,
    exists: impl Fn(&Path) -> bool,
    inherited: &Inherited,
) -> Vec<Exposure> {
    // A definition the caller may not read arrives with its paths left empty.
    if workload.executable.is_empty() {
        return vec![];
    }
    let program = Exposure {
        path: workload.executable.clone().into(),
        what: Use::Program,
    };
    let program_folder = program.path.parent().map(|folder| Exposure {
        path: folder.to_path_buf(),
        what: Use::ProgramFolder,
    });
    let args: Vec<String> = workload
        .args
        .iter()
        .map(|arg| expand(arg, |name| variable(workload, inherited, name)))
        .collect();
    let interpreter = runs_commands(&workload.executable);
    let encoded = encoded_payloads(&workload.executable, &args);
    let unresolved_args = args
        .iter()
        .enumerate()
        .filter(|(index, arg)| {
            unresolved(arg) || (interpreter && is_command(arg)) || encoded.contains(index)
        })
        .map(|(_, arg)| arg)
        .map(|arg| Exposure {
            path: arg.into(),
            what: Use::Unresolved,
        });
    let searched_folders = search_folders(workload, inherited.path.clone());
    let commands = if interpreter {
        commands_found(workload, inherited, &args, &searched_folders, &exists)
    } else {
        vec![]
    };
    let arguments: Vec<Exposure> =
        path_arguments(&args, Path::new(&workload.working_directory), &exists)
            .into_iter()
            .chain(commands)
            .map(|path| Exposure {
                path,
                what: Use::Argument,
            })
            .collect();
    let script_folders: Vec<Exposure> = arguments
        .iter()
        .filter(|exposure| is_code(&exposure.path))
        .filter_map(|exposure| exposure.path.parent())
        .map(|folder| Exposure {
            path: folder.to_path_buf(),
            what: Use::ScriptFolder,
        })
        .collect();
    let folder = Exposure {
        path: workload.working_directory.clone().into(),
        what: Use::Folder,
    };
    let module_folders = module_variables(&workload.executable)
        .iter()
        .filter_map(|name| variable(workload, inherited, name))
        .flat_map(|list| folders_in(&list, Path::new(&workload.working_directory)));
    // A machine PATH entry may name a variable K3 Up couldn't expand; its folder is unknown.
    let searched = searched_folders
        .into_iter()
        .chain(module_folders)
        .map(|path| Exposure {
            what: if unresolved(&path.to_string_lossy()) {
                Use::Unresolved
            } else {
                Use::SearchPath
            },
            path,
        });
    let mut found: Vec<Exposure> = vec![];
    for candidate in std::iter::once(program)
        .chain(program_folder)
        .chain(arguments)
        .chain(script_folders)
        .chain(unresolved_args)
        .chain(std::iter::once(folder))
        .chain(searched)
    {
        let checkable = candidate.what == Use::Unresolved || candidate.path.is_absolute();
        if checkable && found.iter().all(|seen| seen.path != candidate.path) {
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
        inspected(workload, |path| path.exists(), &Inherited::local_system())
            .into_iter()
            .filter(|exposure| {
                exposure.what == Use::Unresolved
                    || crate::win32::changeable_by_others(&exposure.path)
            })
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

pub fn unresolved_line(name: &str, text: &Path) -> String {
    format!(
        "Warning: {name} runs as SYSTEM and uses {}, which K3 Up can't resolve to a path to check. Make sure only administrators can change what it names.",
        text.display()
    )
}

pub fn warning_lines(workload: &Workload, exposed: &[Exposure]) -> Vec<String> {
    exposed
        .iter()
        .map(|exposure| match exposure.what {
            Use::Unresolved => unresolved_line(&workload.name, &exposure.path),
            _ => warning_line(&workload.name, &exposure.path),
        })
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
    fn command_strings_are_split_into_the_words_a_shell_runs() {
        let separator = std::path::MAIN_SEPARATOR;
        let root = absolute("k3up-words");
        let folder = root.join("cwd").join("work");
        let script = root.join("open").join("job.cmd");
        let second = folder.join("next.cmd");
        let spaced = root.join("Program Files").join("app.toml");
        let existing = [script.clone(), second.clone(), spaced.clone()];
        let exists = |path: &Path| existing.iter().any(|known| known == path);
        let call = format!("call ..{separator}..{separator}open{separator}job.cmd && next.cmd");
        assert_eq!(
            path_arguments(&["-c".into(), call], &folder, exists),
            [script, second]
        );
        // A path with a space that exists as written is taken whole, not split.
        assert_eq!(path_arguments(&[text(&spaced)], &folder, exists), [spaced]);
        assert_eq!(
            words(r#"call "C:\a b\x.cmd" & 'y.ps1'|z>out.txt"#),
            ["call", r"C:\a b\x.cmd", "y.ps1", "z", "out.txt"]
        );
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
        let found = inspected(&workload, |_| true, &Inherited::default());
        let paths: Vec<_> = found
            .iter()
            .map(|exposure| (exposure.path.clone(), exposure.what))
            .collect();
        assert_eq!(
            paths,
            [
                (program, Use::Program),
                (std::env::temp_dir(), Use::ProgramFolder),
                (script.clone(), Use::Argument),
                (folder, Use::Argument),
            ]
        );
        let lines = warning_lines(&workload, &found[2..3]);
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
            executable: text(&absolute("tool.exe")),
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
        let found = inspected(&workload, |_| false, &Inherited::default());
        assert!(found.iter().any(|exposure| exposure.what == Use::SearchPath
            && exposure.path == folder.join("local").join("bin")));
    }

    #[test]
    fn variables_expand_as_cmd_expands_them() {
        let lookup = |name: &str| match name.to_ascii_uppercase().as_str() {
            "PUBLIC" => Some("P".to_string()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        assert_eq!(expand("%PUBLIC%\\job.cmd", lookup), "P\\job.cmd");
        assert_eq!(expand("%public%/a/%Public%", lookup), "P/a/P");
        assert_eq!(expand("%UNKNOWN%\\x%PUBLIC%", lookup), "%UNKNOWN%\\xP");
        assert_eq!(expand("100% sure %EMPTY%.", lookup), "100% sure .");
        assert_eq!(expand("%%PUBLIC%", lookup), "%P");
        assert_eq!(expand("plain", lookup), "plain");
    }

    #[test]
    fn arguments_are_read_after_expansion_with_the_workloads_own_values_first() {
        let public = absolute("public");
        let own = absolute("own");
        let mut workload = Workload {
            name: "web".into(),
            executable: text(&absolute("cmd.exe")),
            working_directory: text(&absolute("work")),
            args: vec!["-c".into(), "%PUBLIC%/job.cmd".into()],
            ..Default::default()
        };
        let inherited = Inherited {
            path: None,
            variables: vec![("PUBLIC".into(), text(&public))],
            machine: None,
        };
        let arguments = |workload: &Workload| -> Vec<PathBuf> {
            inspected(workload, |_| false, &inherited)
                .into_iter()
                .filter(|exposure| exposure.what == Use::Argument)
                .map(|exposure| exposure.path)
                .collect()
        };
        assert_eq!(arguments(&workload), [public.join("job.cmd")]);
        workload.environment.insert("Public".into(), text(&own));
        assert_eq!(arguments(&workload), [own.join("job.cmd")]);
    }

    #[test]
    fn delayed_variables_expand_and_unknown_ones_stay_unresolved() {
        let lookup = |name: &str| (name.eq_ignore_ascii_case("OPEN")).then(|| "O".to_string());
        assert_eq!(expand("!OPEN!\\job.cmd", lookup), "O\\job.cmd");
        assert_eq!(expand("%OPEN%!open!", lookup), "OO");
        for text in [
            "%MISSING%\\job.cmd",
            "!MISSING!\\job.cmd",
            "%OPEN:~0,3%\\job.cmd",
            "$env:OPEN\\job.ps1",
            "${env:OPEN}\\job.ps1",
        ] {
            assert!(unresolved(&expand(text, lookup)), "{text}");
        }
        for text in ["100% sure", "Hi! Bye!", "plain", "O\\job.cmd"] {
            assert!(!unresolved(text), "{text}");
        }
    }

    #[test]
    fn unresolved_arguments_are_always_reported() {
        let workload = Workload {
            name: "web".into(),
            executable: text(&absolute("cmd.exe")),
            working_directory: text(&absolute("work")),
            args: vec!["-c".into(), "!MISSING!\\job.cmd".into()],
            ..Default::default()
        };
        let found = inspected(&workload, |_| false, &Inherited::default());
        let unresolved: Vec<_> = found
            .iter()
            .filter(|exposure| exposure.what == Use::Unresolved)
            .collect();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(
            warning_lines(&workload, &[unresolved[0].clone()]),
            [unresolved_line("web", Path::new("!MISSING!\\job.cmd"))]
        );
    }

    #[test]
    fn command_strings_given_to_interpreters_are_always_reported() {
        let workload = |program: &str, args: &[&str]| Workload {
            name: "web".into(),
            executable: text(&absolute(program)),
            working_directory: text(&absolute("work")),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            ..Default::default()
        };
        let unresolved = |workload: &Workload| -> Vec<PathBuf> {
            inspected(workload, |_| false, &Inherited::default())
                .into_iter()
                .filter(|exposure| exposure.what == Use::Unresolved)
                .map(|exposure| exposure.path)
                .collect()
        };
        assert_eq!(
            unresolved(&workload("cmd.exe", &["/c", "call job.cmd ^& next"])),
            [PathBuf::from("call job.cmd ^& next")]
        );
        assert_eq!(
            unresolved(&workload("PowerShell.EXE", &["-Command", "& $script"])),
            [PathBuf::from("& $script")]
        );
        let words = [
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "job.ps1",
        ];
        assert!(unresolved(&workload("powershell.exe", &words)).is_empty());
        assert!(unresolved(&workload("tool.exe", &["a b"])).is_empty());
    }

    #[test]
    fn bare_commands_are_found_in_the_working_folder_then_on_the_path() {
        let work = absolute("work");
        let bin = absolute("bin");
        let in_bin = bin.join("helper.CMD");
        let in_work = work.join("local.exe");
        let existing = [in_bin.clone(), in_work.clone()];
        let exists = |path: &Path| existing.iter().any(|known| known == path);
        let mut workload = Workload {
            name: "web".into(),
            executable: text(&absolute("cmd.exe")),
            working_directory: text(&work),
            args: vec!["-c".into(), "helper & local.exe && missing".into()],
            ..Default::default()
        };
        workload
            .environment
            .insert("PATHEXT".into(), ".EXE;.CMD".into());
        let inherited = Inherited {
            path: Some(text(&bin)),
            variables: vec![],
            machine: None,
        };
        let found = commands_found(
            &workload,
            &inherited,
            &workload.args,
            &search_folders(&workload, inherited.path.clone()),
            &exists,
        );
        assert_eq!(found, [in_bin.clone(), in_work]);
        let folders: Vec<_> = inspected(&workload, exists, &inherited)
            .into_iter()
            .filter(|exposure| exposure.what == Use::ScriptFolder)
            .map(|exposure| exposure.path)
            .collect();
        assert_eq!(folders, [work, bin]);
    }

    #[test]
    fn encoded_powershell_commands_are_always_reported() {
        let payload = "SQBuAHYAbwBrAGUA".to_string();
        let args = |switch: &str| vec!["-NoProfile".to_string(), switch.into(), payload.clone()];
        for switch in [
            "-EncodedCommand",
            "-enc",
            "-e",
            "-EC",
            "/encodedcommand",
            "-EncodedArguments",
        ] {
            assert_eq!(encoded_payloads("pwsh.exe", &args(switch)), [2], "{switch}");
        }
        for switch in ["-ExecutionPolicy", "-File", "-enco_x"] {
            assert!(
                encoded_payloads("powershell.exe", &args(switch)).is_empty(),
                "{switch}"
            );
        }
        assert!(encoded_payloads("tool.exe", &args("-enc")).is_empty());
        assert!(encoded_payloads("powershell.exe", &["-enc".into()]).is_empty());
    }

    #[test]
    fn machine_variables_and_module_paths_are_followed() {
        // The registry finds values whatever the case of their names.
        fn machine(name: &str) -> Option<String> {
            match name.to_ascii_uppercase().as_str() {
                "TOOLS" => Some("%ROOT%\\tools".into()),
                "PYTHONPATH" => Some("%ROOT%\\site".into()),
                _ => None,
            }
        }
        let root = text(&absolute("root"));
        let inherited = Inherited {
            path: None,
            variables: vec![("ROOT".into(), root.clone())],
            machine: Some(machine),
        };
        assert_eq!(inherited.value("tools"), Some(format!("{root}\\tools")));
        assert_eq!(inherited.value("MISSING"), None);

        let python = Workload {
            name: "py".into(),
            executable: text(&absolute("python.exe")),
            working_directory: text(&absolute("work")),
            args: vec!["-m".into(), "helper".into()],
            ..Default::default()
        };
        let searched: Vec<_> = inspected(&python, |_| false, &inherited)
            .into_iter()
            .filter(|exposure| exposure.what == Use::SearchPath)
            .map(|exposure| exposure.path)
            .collect();
        assert!(
            searched.contains(&PathBuf::from(format!("{root}\\site"))),
            "{searched:?}"
        );

        let mut tool = python.clone();
        tool.executable = text(&absolute("tool.exe"));
        let unknown = Inherited {
            path: Some("%NOT_SET%\\bin".into()),
            ..inherited.clone()
        };
        let found = inspected(&tool, |_| false, &unknown);
        assert!(
            found
                .iter()
                .any(|exposure| exposure.what == Use::Unresolved)
        );
        assert!(!found.iter().any(|exposure| exposure.path.ends_with("site")));
    }

    #[test]
    fn a_definition_the_caller_cannot_read_has_nothing_to_inspect() {
        let hidden = Workload {
            name: "web".into(),
            ..Default::default()
        };
        assert!(inspected(&hidden, |_| true, &Inherited::default()).is_empty());
        assert!(
            inspected(
                &hidden,
                |_| true,
                &Inherited {
                    path: Some(std::env::temp_dir().display().to_string()),
                    variables: vec![],
                    machine: None,
                },
            )
            .is_empty()
        );
        assert!(exposed_paths(&hidden).is_empty());
    }
}
