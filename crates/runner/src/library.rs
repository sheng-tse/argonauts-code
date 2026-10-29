//! The workflows on disk and the built-in ones: the project's `.argonauts/workflows/`, then
//! `~/.argonauts/workflows/`, then what ships with Argonauts Code.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::file::{FileError, WorkflowFile};
use crate::script::{Script, ScriptError};

/// What `workflows/README.md` says: every key of a step, and what a script may use.
pub use argonauts_core::workflow_files::README;
/// The built-in workflows that have steps, as `(name, the file's text)`. Captain has none: it
/// is the CLI itself.
pub use argonauts_core::workflow_files::with_steps as built_ins;

const NAME_LIMIT: usize = 40;
const STEPS: &str = "toml";
const SCRIPTS: [&str; 2] = ["js", "mjs"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// `.argonauts/workflows/` in the repository.
    Project,
    /// `~/.argonauts/workflows/`.
    Yours,
    BuiltIn,
}

/// A workflow ready to run.
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded {
    Steps(WorkflowFile),
    Script(Script),
}

impl Loaded {
    pub fn name(&self) -> &str {
        match self {
            Loaded::Steps(file) => &file.name,
            Loaded::Script(script) => &script.meta.name,
        }
    }

    pub fn about(&self) -> &str {
        match self {
            Loaded::Steps(file) => &file.about,
            Loaded::Script(script) => &script.meta.description,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub name: String,
    pub origin: Origin,
    /// `None` for a built-in one nobody wrote out.
    pub path: Option<PathBuf>,
    /// One line on what it's for.
    pub about: String,
    /// Why it can't run as it is.
    pub problem: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("there's no workflow named {0}. /workflow lists them")]
    Unknown(String),
    #[error("{}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("{name}: {source}")]
    File { name: String, source: FileError },
    #[error("{name}: {source}")]
    Script { name: String, source: ScriptError },
    #[error("{0}")]
    Name(String),
    #[error(
        "{name} has no steps. It only sets settings for the built-in way of working, so there's \
         nothing to try or run by itself"
    )]
    NoSteps { name: String },
}

/// The two folders a workflow of your own may sit in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    pub project: Option<PathBuf>,
    pub yours: Option<PathBuf>,
    /// The project's files that came with the repository and wait for your trust. They are
    /// passed over as if they weren't there, so one named after a built-in leaves it in place.
    pub waiting: Vec<PathBuf>,
}

impl Library {
    /// Every workflow, each name once: the project's, then yours, then the built-in ones.
    pub fn list(&self) -> Vec<Listed> {
        let mut listed: Vec<Listed> = Vec::new();
        let folders = [
            (Origin::Project, &self.project),
            (Origin::Yours, &self.yours),
        ];
        for (origin, folder) in folders {
            let Some(folder) = folder else { continue };
            for (name, path) in files_in(folder, &self.waiting) {
                if listed.iter().any(|known| known.name == name) {
                    continue;
                }
                let (about, problem) = match read(&name, &path) {
                    Ok(Some(loaded)) => (loaded.about().to_owned(), None),
                    // A settings-only file isn't a workflow of its own to list.
                    Ok(None) => continue,
                    Err(err) => (String::new(), Some(err.to_string())),
                };
                listed.push(Listed {
                    name,
                    origin,
                    path: Some(path),
                    about,
                    problem,
                });
            }
        }
        for (name, text) in built_ins() {
            if listed.iter().any(|known| known.name == name) {
                continue;
            }
            let about = WorkflowFile::parse(text)
                .ok()
                .flatten()
                .map(|file| file.about)
                .unwrap_or_default();
            listed.push(Listed {
                name: name.to_owned(),
                origin: Origin::BuiltIn,
                path: None,
                about,
                problem: None,
            });
        }
        listed
    }

    /// Where a workflow of this name is, and its text.
    pub fn find(&self, name: &str) -> Option<(Origin, Option<PathBuf>)> {
        let folders = [
            (Origin::Project, &self.project),
            (Origin::Yours, &self.yours),
        ];
        for (origin, folder) in folders {
            let Some(folder) = folder else { continue };
            if let Some((_, path)) = files_in(folder, &self.waiting)
                .into_iter()
                .find(|(found, _)| found == name)
            {
                return Some((origin, Some(path)));
            }
        }
        built_in(name).map(|_| (Origin::BuiltIn, None))
    }

    /// The workflow, read and checked.
    pub fn load(&self, name: &str) -> Result<Loaded, LibraryError> {
        let unknown = || LibraryError::Unknown(name.to_owned());
        let (_, path) = self.find(name).ok_or_else(unknown)?;
        let loaded = match path {
            Some(path) => read(name, &path)?,
            None => {
                let text = built_in(name).ok_or_else(unknown)?;
                parse_steps(name, text)?.map(Loaded::Steps)
            }
        };
        // A file of your own that only sets settings leaves the steps to the built-in of its
        // name, if there is one.
        match loaded {
            Some(loaded) => Ok(loaded),
            None => match built_in(name) {
                Some(text) => parse_steps(name, text)?.map(Loaded::Steps).ok_or_else(|| {
                    LibraryError::NoSteps {
                        name: name.to_owned(),
                    }
                }),
                None => Err(LibraryError::NoSteps {
                    name: name.to_owned(),
                }),
            },
        }
    }

    /// Writes a new workflow into the project's folder, or yours when there's no project:
    /// the commented template, or a copy of the built-in `from`. `taken` holds the names
    /// commands already have.
    pub fn create(
        &self,
        name: &str,
        from: Option<&str>,
        taken: &[&str],
    ) -> Result<PathBuf, LibraryError> {
        check_name(name, taken)?;
        if self.find(name).is_some() {
            return Err(LibraryError::Name(format!(
                "there's already a workflow named {name}. /workflow edit {name} opens it"
            )));
        }
        let folder = self
            .project
            .as_ref()
            .or(self.yours.as_ref())
            .ok_or_else(|| {
                LibraryError::Name("there's no folder to keep workflows in".to_owned())
            })?;
        let shown = shown_folder(folder);
        let text = match from {
            None => template(name, &shown),
            Some(from) => {
                let source = built_in(from).ok_or_else(|| {
                    let names: Vec<&str> = built_ins().map(|(name, _)| name).collect();
                    LibraryError::Name(format!(
                        "{from} isn't a built-in workflow. The built-in ones are {}",
                        names.join(", ")
                    ))
                })?;
                copy_of(source, name, from, &shown)
            }
        };
        let path = folder.join(format!("{name}.{STEPS}"));
        let failed = |source: io::Error| LibraryError::Read {
            path: path.clone(),
            source,
        };
        fs::create_dir_all(folder).map_err(failed)?;
        fs::write(&path, text).map_err(failed)?;
        let readme = folder.join("README.md");
        if !readme.exists() {
            fs::write(&readme, README).map_err(|source| LibraryError::Read {
                path: readme.clone(),
                source,
            })?;
        }
        Ok(path)
    }
}

/// A built-in workflow's file by name, one of phases alone among them: Council talks in its
/// phases, so it has no steps, yet it is there to copy and to name.
pub fn built_in(name: &str) -> Option<&'static str> {
    argonauts_core::workflow_files::built_in(name)
}

/// A workflow's name is a command, so it is lowercase letters, digits and dashes, and never a
/// name a command already has.
pub fn check_name(name: &str, taken: &[&str]) -> Result<(), LibraryError> {
    let plain = !name.is_empty()
        && name.len() <= NAME_LIMIT
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !plain {
        return Err(LibraryError::Name(format!(
            "a workflow's name is its command, so it takes lowercase letters, digits and dashes, \
             up to {NAME_LIMIT}: {name:?} doesn't fit"
        )));
    }
    if taken.contains(&name) {
        return Err(LibraryError::Name(format!(
            "/{name} is already a command, so a workflow can't take that name. Try {name}-flow"
        )));
    }
    Ok(())
}

/// The workflow files in a folder, by name, in name order, leaving out those that wait for
/// your trust. A script wins over a file of steps of the same name, since it says more.
fn files_in(folder: &Path, waiting: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let (Some(stem), Some(extension)) = (
            path.file_stem().and_then(|stem| stem.to_str()),
            path.extension().and_then(|extension| extension.to_str()),
        ) else {
            continue;
        };
        let script = SCRIPTS.contains(&extension);
        if (!script && extension != STEPS) || waiting.contains(&path) {
            continue;
        }
        // The steps every workflow must contain sit beside the workflows, and are none.
        if path.file_name().and_then(|name| name.to_str()) == Some(argonauts_core::required::FILE) {
            continue;
        }
        match found.iter_mut().find(|(name, _)| name == stem) {
            Some(known) if script => known.1 = path.clone(),
            Some(_) => {}
            None => found.push((stem.to_owned(), path.clone())),
        }
    }
    found
}

/// `Ok(None)` is a file that only carries settings.
fn read(name: &str, path: &Path) -> Result<Option<Loaded>, LibraryError> {
    let text = fs::read_to_string(path).map_err(|source| LibraryError::Read {
        path: path.to_owned(),
        source,
    })?;
    let script = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SCRIPTS.contains(&extension));
    if script {
        return Script::parse(&text)
            .map(|script| Some(Loaded::Script(script)))
            .map_err(|source| LibraryError::Script {
                name: name.to_owned(),
                source,
            });
    }
    Ok(parse_steps(name, &text)?.map(Loaded::Steps))
}

/// A file's steps, named after the file when it doesn't name itself.
fn parse_steps(name: &str, text: &str) -> Result<Option<WorkflowFile>, LibraryError> {
    let file = WorkflowFile::parse(text).map_err(|source| LibraryError::File {
        name: name.to_owned(),
        source,
    })?;
    Ok(file.map(|mut file| {
        if file.name.is_empty() {
            name.clone_into(&mut file.name);
        }
        file
    }))
}

/// The folder as its files' header names it: `.argonauts/workflows`, or `~/.argonauts/workflows`.
fn shown_folder(folder: &Path) -> String {
    let parts: Vec<String> = folder
        .components()
        .rev()
        .take(2)
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// What `/workflow new <name>` writes.
pub fn template(name: &str, folder: &str) -> String {
    format!(
        r#"# {folder}/{name}.toml · made by /workflow new {name}
# Use it once with /{name} <your request>, or always with /workflow {name}.
name  = "{name}"
about = "One line that says what this workflow is for."
adapt = false            # false: the steps below run exactly as written · true: the orchestrator may add to them
# goal = "every test passes"         # optional: go round until this holds

[[step]]
name = "Plan"
role = "orchestrator"    # orchestrator · worker · reviewer · judge
does = "Split the request into tasks: {{request}}"

[[step]]
name       = "Build"
role       = "worker"
for_each   = "task"      # one agent per task, side by side
max_rounds = 3           # the first try and two returns from Review, then it stops
# until    = "its own tests pass"    # optional: a goal on this step

[[step]]
name         = "Review"
role         = "reviewer"
for_each     = "task"
send_back_to = "Build"   # until the reviewer accepts

# Also on a step: count, independent, not_the_author, then, model, kept_by. README.md here explains each.
"#
    )
}

/// A built-in workflow's file under a new name.
fn copy_of(source: &str, name: &str, from: &str, folder: &str) -> String {
    let mut text = format!(
        "# {folder}/{name}.toml · made by /workflow new {name} from {from}\n# Use it once with \
         /{name} <your request>, or always with /workflow {name}.\n"
    );
    for line in source.lines() {
        let key = line.split('=').next().unwrap_or_default().trim();
        if key == "name" && !line.starts_with(char::is_whitespace) && !text.contains("\nname") {
            text.push_str(&format!("name  = \"{name}\"\n"));
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(dir: &Path) -> Library {
        Library {
            project: Some(dir.join("repo/.argonauts/workflows")),
            yours: Some(dir.join("home/.argonauts/workflows")),
            waiting: Vec::new(),
        }
    }

    #[test]
    fn a_project_file_that_waits_for_trust_leaves_the_built_in_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = library(dir.path());
        let project = library.project.clone().unwrap();
        fs::create_dir_all(&project).unwrap();
        let mine = project.join("maker-checker.toml");
        fs::write(
            &mine,
            "name = \"maker-checker\"\n[[step]]\nname = \"Do\"\nrole = \"worker\"\n",
        )
        .unwrap();
        assert_eq!(
            library.find("maker-checker").map(|found| found.0),
            Some(Origin::Project)
        );
        library.waiting = vec![mine];
        assert_eq!(library.find("maker-checker"), Some((Origin::BuiltIn, None)));
        let listed = library.list();
        let kept: Vec<&Listed> = listed
            .iter()
            .filter(|listed| listed.name == "maker-checker")
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].origin, Origin::BuiltIn);
    }

    #[test]
    fn every_built_in_reads_and_names_itself() {
        for (name, text) in built_ins() {
            let file = WorkflowFile::parse(text)
                .unwrap_or_else(|err| panic!("{name}: {err}"))
                .expect("has steps");
            assert_eq!(file.name, name);
            assert!(file.adapt, "{name} lets the orchestrator write the steps");
            assert!(
                !file.about.is_empty() && !file.about.contains('\n'),
                "{name}"
            );
        }
        let listed = Library::default().list();
        let names: Vec<&str> = listed.iter().map(|listed| listed.name.as_str()).collect();
        assert_eq!(names, ["orchestrator", "maker-checker", "best-of-n"]);
        // Council talks in its phases, so it has no steps to list, yet it is there by name.
        assert_eq!(
            Library::default().find("council"),
            Some((Origin::BuiltIn, None))
        );
        assert!(matches!(
            Library::default().load("council"),
            Err(LibraryError::NoSteps { .. })
        ));
    }

    #[test]
    fn new_writes_the_template_which_reads_back_and_a_readme_beside_it() {
        let dir = tempfile::tempdir().expect("a folder");
        let library = library(dir.path());
        let path = library
            .create("spec-first", None, &["model"])
            .expect("writes");
        assert!(path.ends_with("repo/.argonauts/workflows/spec-first.toml"));
        let text = fs::read_to_string(&path).expect("reads");
        assert!(
            text.starts_with(
                "# .argonauts/workflows/spec-first.toml · made by /workflow new spec-first\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("does = \"Split the request into tasks: {request}\""),
            "{text}"
        );
        let Loaded::Steps(file) = library.load("spec-first").expect("loads") else {
            panic!("a file of steps");
        };
        assert_eq!(
            (file.name.as_str(), file.adapt, file.steps.len()),
            ("spec-first", false, 3)
        );
        assert!(path.with_file_name("README.md").exists());
        let listed = library.list();
        assert_eq!(listed[0].name, "spec-first");
        assert_eq!(listed[0].origin, Origin::Project);
        assert_eq!(
            listed[0].about,
            "One line that says what this workflow is for."
        );
    }

    #[test]
    fn a_name_a_command_has_or_one_that_is_not_a_command_name_is_refused() {
        let dir = tempfile::tempdir().expect("a folder");
        let library = library(dir.path());
        let taken = library
            .create("model", None, &["model", "help"])
            .unwrap_err();
        assert_eq!(
            taken.to_string(),
            "/model is already a command, so a workflow can't take that name. Try model-flow"
        );
        assert!(library.create("Spec First", None, &[]).is_err());
        assert!(
            library.create("council", None, &[]).is_err(),
            "a built-in's name is taken"
        );
        library.create("mine", None, &[]).expect("writes");
        let twice = library.create("mine", None, &[]).unwrap_err();
        assert!(
            twice.to_string().contains("already a workflow named mine"),
            "{twice}"
        );
    }

    #[test]
    fn new_from_a_built_in_copies_it_under_the_new_name() {
        let dir = tempfile::tempdir().expect("a folder");
        let library = library(dir.path());
        library
            .create("my-checker", Some("maker-checker"), &[])
            .expect("writes");
        let Loaded::Steps(file) = library.load("my-checker").expect("loads") else {
            panic!("a file of steps");
        };
        assert_eq!(file.name, "my-checker");
        assert_eq!(file.steps.len(), 2);
        // The copy is the whole workflow: its phases come with its steps, under the new name.
        let written = dir.path().join("repo/.argonauts/workflows/my-checker.toml");
        let text = fs::read_to_string(written).expect("reads");
        let phases = argonauts_core::workflow_files::phases(&text, "the-file")
            .expect("reads")
            .expect("has phases");
        assert_eq!(phases.name, "my-checker");
        assert_eq!(phases.phases.len(), 3);
        // A council of your own talks in its phases like the built-in.
        library
            .create("my-council", Some("council"), &[])
            .expect("writes");
        let written = dir.path().join("repo/.argonauts/workflows/my-council.toml");
        let text = fs::read_to_string(written).expect("reads");
        let phases = argonauts_core::workflow_files::phases(&text, "the-file")
            .expect("reads")
            .expect("has phases");
        assert_eq!(phases.name, "my-council");
        assert_eq!(phases.phases[1].name, "Talk");
        let unknown = library
            .create("other", Some("no-such-workflow"), &[])
            .unwrap_err();
        assert!(
            unknown.to_string().contains("isn't a built-in workflow"),
            "{unknown}"
        );
    }

    /// `always.toml` holds the steps every workflow must contain, so it is none of them.
    #[test]
    fn the_steps_every_workflow_must_contain_are_not_a_workflow() {
        let dir = tempfile::tempdir().expect("a folder");
        let library = library(dir.path());
        let yours = library.yours.clone().unwrap();
        fs::create_dir_all(&yours).unwrap();
        fs::write(
            yours.join(argonauts_core::required::FILE),
            "[[step]]\nname = \"benchmark\"\nrole = \"worker\"\n",
        )
        .unwrap();
        assert!(library.list().iter().all(|listed| listed.name != "always"));
        assert!(library.find("always").is_none());
    }

    #[test]
    fn the_project_wins_over_yours_and_a_script_is_a_workflow_too() {
        let dir = tempfile::tempdir().expect("a folder");
        let library = library(dir.path());
        let (project, yours) = (
            library.project.clone().unwrap(),
            library.yours.clone().unwrap(),
        );
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&yours).unwrap();
        let steps = |about: &str| {
            format!("about = \"{about}\"\n[[step]]\nname = \"Do\"\nrole = \"worker\"\n")
        };
        fs::write(project.join("ship.toml"), steps("the project's")).unwrap();
        fs::write(yours.join("ship.toml"), steps("yours")).unwrap();
        fs::write(
            yours.join("sweep.js"),
            "export const meta = { name: 'sweep', description: 'Sweep the repo' }\nreturn 1\n",
        )
        .unwrap();
        fs::write(yours.join("broken.toml"), "[[step]]\nname = \"Do\"\n").unwrap();
        fs::write(yours.join("tuned.toml"), "[review]\nper_task = 3\n").unwrap();
        let listed = library.list();
        let shown: Vec<(&str, Origin, &str)> = listed
            .iter()
            .take(3)
            .map(|listed| (listed.name.as_str(), listed.origin, listed.about.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("ship", Origin::Project, "the project's"),
                ("broken", Origin::Yours, ""),
                ("sweep", Origin::Yours, "Sweep the repo"),
            ]
        );
        assert_eq!(
            listed[1].problem.as_deref(),
            Some("broken: step 1 (Do): it needs a role")
        );
        assert!(listed.iter().all(|listed| listed.name != "tuned"));
        assert!(matches!(library.load("sweep"), Ok(Loaded::Script(_))));
        assert_eq!(library.load("ship").expect("loads").name(), "ship");
        assert!(matches!(
            library.load("tuned"),
            Err(LibraryError::NoSteps { .. })
        ));
        assert!(matches!(
            library.load("nope"),
            Err(LibraryError::Unknown(_))
        ));
    }
}
