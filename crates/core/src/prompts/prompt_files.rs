//! Your own prompt files. A template in `.argonauts/prompts/` takes the place of the built-in
//! of the same name, and a file in `.argonauts/roles/` adds to what a role is told, or
//! replaces it. The project's folder is read first, then `~/.argonauts/`. Deleting a file goes
//! back to the built-in.
//!
//! A template Argonauts Code wrote out is a copy until you edit it: it follows the built-in, so a
//! newer version's wording isn't held back by a file nobody changed.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use crate::base::hash::sha256_hex;
use crate::base::names::RoleName;
use crate::prompts::prompt_parts::Readers;
use crate::settings::crew::CrewRole;

/// The project's own folder, in the repository.
pub const PROJECT_DIR: &str = ".argonauts";
pub const PROMPTS_DIR: &str = "prompts";
pub const ROLES_DIR: &str = "roles";

/// One built-in prompt: a role's frame, or Argonauts Code's own instructions for one step. Your
/// file `prompts/<id>.md` takes its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Template {
    /// Its file under `prompts/`, without `.md`: `review/review`, `frames/reviewer`.
    pub id: &'static str,
    /// The crew role whose agents read it.
    pub role: CrewRole,
    /// What it is, in a few words, for `/prompts`: `frame`, `review a change`.
    pub part: &'static str,
    pub text: &'static str,
}

impl Template {
    pub fn is_frame(&self) -> bool {
        self.id.starts_with(FRAMES)
    }
}

/// Frames live together, one per crew role.
pub const FRAMES: &str = "frames/";

/// Where your files are looked for, the project first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptDirs {
    /// The project's `.argonauts`, in your checkout.
    pub project: Option<PathBuf>,
    /// `~/.argonauts`.
    pub home: Option<PathBuf>,
    /// Project files that came with the repository and wait for your trust. They are passed
    /// over as if they weren't there.
    pub waiting: BTreeSet<PathBuf>,
}

/// One of your files, as it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YourFile {
    pub text: String,
    pub path: PathBuf,
    /// The path the screen shows: `roles/reviewer.md` for the project's, and
    /// `~/.argonauts/roles/reviewer.md` for yours everywhere.
    pub shown: String,
}

/// Whether your file for a role is added after the role's instructions or takes their place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoleFileMode {
    #[default]
    Add,
    Replace,
}

/// Your instructions for a role, read from `roles/<role>.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleFile {
    pub mode: RoleFileMode,
    /// The text without its front matter.
    pub text: String,
    pub shown: String,
    pub path: PathBuf,
}

impl PromptDirs {
    /// `repo` is your checkout's root and `home` is `~/.argonauts`.
    pub fn new(repo: Option<&Path>, home: Option<&Path>) -> Self {
        Self {
            project: repo.map(|repo| repo.join(PROJECT_DIR)),
            home: home.map(Path::to_path_buf),
            waiting: BTreeSet::new(),
        }
    }

    /// The same folders with `waiting` passed over until you trust them.
    pub fn without(mut self, waiting: impl IntoIterator<Item = PathBuf>) -> Self {
        self.waiting = waiting.into_iter().collect();
        self
    }

    /// Every roles and prompts file in the project's folder that an agent may come to read,
    /// whoever wrote it. The README is for you, not for an agent, and a copy of a template
    /// nobody edited is never read, so neither is one of them.
    pub fn project_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Some(project) = &self.project {
            markdown_files(&project.join(ROLES_DIR), &mut files);
            let prompts = project.join(PROMPTS_DIR);
            let mut templates = Vec::new();
            markdown_files(&prompts, &mut templates);
            let copies = Copies::read(&prompts);
            templates.retain(|path| !copies.holds_file(&prompts, path));
            files.extend(templates);
        }
        files.sort();
        files
    }

    /// Your template for the prompt `id`, such as `review/review` or `frames/reviewer`. A
    /// missing or blank file is no template, so the built-in is used, and neither is a copy
    /// Argonauts Code wrote that you never edited.
    pub fn template(&self, id: &str) -> Option<YourFile> {
        self.places()
            .find_map(|place| place.template(id))
            .filter(|file| !file.text.trim().is_empty())
    }

    /// The file that stands at the template's name, edited or not: what `/prompts` marks,
    /// compares and deletes.
    pub fn template_file(&self, id: &str) -> Option<YourFile> {
        let relative = format!("{PROMPTS_DIR}/{id}.md");
        self.places().find_map(|place| place.read(&relative))
    }

    /// Your file for `role`: the role's own name first (`roles/critic.md`), then the crew role
    /// it follows (`roles/reviewer.md`). `Err` says why a file that's there can't be used.
    pub fn role(&self, role: RoleName) -> Option<Result<RoleFile, String>> {
        let names = [role.as_str(), CrewRole::read_by(role).as_str()];
        self.places().find_map(|place| {
            names.iter().find_map(|name| {
                let file = place.read(&format!("{ROLES_DIR}/{name}.md"))?;
                parse_role_file(file).transpose()
            })
        })
    }

    /// Your file under the readers' own name, such as `roles/reviewer.md` for the crew role
    /// as a whole or `roles/critic.md` for the critic alone.
    pub fn own_role(&self, readers: impl Into<Readers>) -> Option<Result<RoleFile, String>> {
        parse_role_file(self.own_role_file(readers)?).transpose()
    }

    /// The file behind [`Self::own_role`] as it is on disk, whether or not it can be used:
    /// what `/prompts` deletes. A starter nobody wrote in is no role file here either.
    pub fn own_role_file(&self, readers: impl Into<Readers>) -> Option<YourFile> {
        let relative = format!("{ROLES_DIR}/{}.md", readers.into().as_str());
        self.places().find_map(|place| {
            let file = place.read(&relative)?;
            let says_nothing = matches!(parse_role_file(file.clone()), Ok(None));
            (!says_nothing).then_some(file)
        })
    }

    /// Every file of yours under `folder` (`roles` or `prompts`) in both places, the
    /// project's first, each by its name in the folder without `.md`, such as
    /// `frames/reviewer`. The README, a copy of a template nobody edited and a file that
    /// waits for your trust aren't among them.
    pub fn files_in(&self, folder: &str) -> Vec<(String, YourFile)> {
        let mut found = Vec::new();
        for place in self.places() {
            let dir = place.dir.join(folder);
            let mut paths = Vec::new();
            markdown_files(&dir, &mut paths);
            paths.sort();
            // Only templates are ever copies. A role's file is yours from its first word.
            let copies = if folder == PROMPTS_DIR {
                Copies::read(&dir)
            } else {
                Copies::default()
            };
            for path in paths {
                let name = path.strip_prefix(&dir).ok().and_then(Path::to_str);
                let Some(name) = name.and_then(|name| name.strip_suffix(".md")) else {
                    continue;
                };
                let file = place.read(&format!("{folder}/{name}.md"));
                let Some(file) =
                    file.filter(|file| !copies.holds(&format!("{name}.md"), &file.text))
                else {
                    continue;
                };
                found.push((name.to_owned(), file));
            }
        }
        found
    }

    /// The folder a new file of yours is written to: the project's when there is one.
    pub fn write_dir(&self) -> Option<&Path> {
        self.project.as_deref().or(self.home.as_deref())
    }

    fn places(&self) -> impl Iterator<Item = Place<'_>> {
        let project = self.project.as_deref().map(|dir| Place {
            dir,
            prefix: "",
            waiting: Some(&self.waiting),
        });
        let home = self.home.as_deref().map(|dir| Place {
            dir,
            prefix: "~/.argonauts/",
            waiting: None,
        });
        project.into_iter().chain(home)
    }
}

fn markdown_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "md")
            && path.file_name().is_some_and(|name| name != README_FILE)
        {
            files.push(path);
        }
    }
}

/// How this process finds your folders, for the few prompts sent from where no run or
/// repository is at hand: the app's own, the judge's and the narrator's. A program says it
/// once when it starts; until then those prompts are the built-in ones.
type Source = Box<dyn Fn() -> PromptDirs + Send + Sync>;

static DIRS: RwLock<Option<Source>> = RwLock::new(None);

/// `source` is asked each time a prompt is sent, so a file that came with a pull while the
/// program runs waits for your trust like any other.
pub fn use_dirs_from(source: impl Fn() -> PromptDirs + Send + Sync + 'static) {
    *DIRS.write().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(source));
}

/// The prompt `id` as this process sends it: your template when you wrote one, else
/// `built_in`.
pub fn in_use(id: &str, built_in: &str) -> String {
    let source = DIRS.read().unwrap_or_else(PoisonError::into_inner);
    source
        .as_ref()
        .and_then(|dirs| dirs().template(id))
        .map_or_else(|| built_in.to_owned(), |yours| yours.text)
}

/// What [`write_templates`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Written {
    pub dir: PathBuf,
    /// Templates written as the built-in has them: the ones that were missing, and the copies
    /// nobody edited of what an earlier version said.
    pub written: Vec<String>,
    /// Files that hold your own text, which were left alone.
    pub kept: Vec<String>,
    /// How many were there already as the built-in has them.
    pub unchanged: usize,
}

/// What became of one file Argonauts Code writes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// Written now, as the built-in has it.
    Fresh,
    /// There already, as the built-in has it.
    AsBuiltIn,
    /// There with your own text, which was left alone.
    Yours,
}

/// Writes every template into `<argonauts_dir>/prompts/`, each as the built-in has it, with a
/// README that says how they are used. A file you edited is yours and stays as it is, so this
/// is safe to run again: all it changes is the copies nobody edited, which it brings up to
/// what the built-ins say now.
pub fn write_templates(argonauts_dir: &Path, templates: &[Template]) -> io::Result<Written> {
    let dir = argonauts_dir.join(PROMPTS_DIR);
    let mut copies = Copies::read(&dir);
    let mut done = Written {
        dir: dir.clone(),
        ..Written::default()
    };
    for template in templates {
        let name = format!("{}.md", template.id);
        match copies.write(&dir, &name, template.text)? {
            Copied::Fresh => done.written.push(name),
            Copied::AsBuiltIn => done.unchanged += 1,
            Copied::Yours => done.kept.push(name),
        }
    }
    copies.write(&dir, README_FILE, &readme(templates))?;
    copies.save(&dir)?;
    Ok(done)
}

/// Writes one template the same way.
pub fn write_template(argonauts_dir: &Path, template: &Template) -> io::Result<Copied> {
    let dir = argonauts_dir.join(PROMPTS_DIR);
    let mut copies = Copies::read(&dir);
    let copied = copies.write(&dir, &format!("{}.md", template.id), template.text)?;
    copies.save(&dir)?;
    Ok(copied)
}

/// What Argonauts Code wrote into one `prompts/` folder: each file's name with the SHA-256 of the
/// text it was given, kept in the folder itself. A file that still holds that text is a copy
/// nobody edited.
///
/// Claiming a file here can only take it out of use, never put its words in front of an
/// agent, so a record that came with a repository needs no trust.
#[derive(Debug, Default)]
struct Copies {
    written: BTreeMap<String, String>,
    changed: bool,
}

const COPIES_FILE: &str = ".written";

const COPIES_HEAD: &str = "# What Argonauts Code wrote in this folder. A file that still reads \
                           as it was written follows the built-in prompt. Edit one and it is \
                           yours.\n";

impl Copies {
    /// An unreadable record is an empty one: every file then counts as yours, which is the
    /// reading that never drops your words.
    fn read(dir: &Path) -> Self {
        let text = std::fs::read_to_string(dir.join(COPIES_FILE)).unwrap_or_default();
        let written = text
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once("  "))
            .map(|(hash, name)| (name.to_owned(), hash.to_owned()))
            .collect();
        Self {
            written,
            changed: false,
        }
    }

    /// Whether the file `name` still holds what Argonauts Code wrote.
    fn holds(&self, name: &str, text: &str) -> bool {
        self.written
            .get(name)
            .is_some_and(|hash| *hash == sha256_hex(text.as_bytes()))
    }

    fn holds_file(&self, dir: &Path, path: &Path) -> bool {
        let Some(name) = path.strip_prefix(dir).ok().and_then(Path::to_str) else {
            return false;
        };
        // Cheaper than reading every file: most folders hold no record at all.
        self.written.contains_key(name)
            && std::fs::read_to_string(path).is_ok_and(|text| self.holds(name, &text))
    }

    /// Writes `text` at `name` unless the file there is yours.
    fn write(&mut self, dir: &Path, name: &str, text: &str) -> io::Result<Copied> {
        let path = dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(there) if there == text => {
                self.note(name, text);
                return Ok(Copied::AsBuiltIn);
            }
            Ok(there) if !self.holds(name, &there) => return Ok(Copied::Yours),
            // Text that isn't UTF-8 is nothing Argonauts Code wrote.
            Err(err) if err.kind() == io::ErrorKind::InvalidData => return Ok(Copied::Yours),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        save(&path, text)?;
        self.note(name, text);
        Ok(Copied::Fresh)
    }

    fn note(&mut self, name: &str, text: &str) {
        let hash = sha256_hex(text.as_bytes());
        if self.written.get(name) != Some(&hash) {
            self.written.insert(name.to_owned(), hash);
            self.changed = true;
        }
    }

    fn save(&self, dir: &Path) -> io::Result<()> {
        if !self.changed {
            return Ok(());
        }
        let mut text = COPIES_HEAD.to_owned();
        for (name, hash) in &self.written {
            text.push_str(&format!("{hash}  {name}\n"));
        }
        save(&dir.join(COPIES_FILE), &text)
    }
}

fn write_new(path: &Path, text: &str) -> io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    Ok(true)
}

pub const README_FILE: &str = "README.md";

const README_HEAD: &str = crate::template!("prompts/README.md");

/// The README beside the templates: how an agent's prompt is put together, then every file
/// with the role that reads it and what it is.
pub fn readme(templates: &[Template]) -> String {
    let mut text = README_HEAD.trim_end().to_owned();
    text.push_str("\n\n## The files\n");
    for role in CrewRole::ALL {
        let of_role: Vec<&Template> = templates.iter().filter(|t| t.role == role).collect();
        if of_role.is_empty() {
            continue;
        }
        text.push_str(&format!("\n{}:\n", role.plural()));
        for template in of_role {
            text.push_str(&format!("- `{}.md`: {}\n", template.id, template.part));
        }
    }
    text
}

struct Place<'a> {
    dir: &'a Path,
    prefix: &'static str,
    waiting: Option<&'a BTreeSet<PathBuf>>,
}

impl Place<'_> {
    /// The template you keep here for the prompt `id`. A copy nobody edited isn't one.
    fn template(&self, id: &str) -> Option<YourFile> {
        let file = self.read(&format!("{PROMPTS_DIR}/{id}.md"))?;
        let copies = Copies::read(&self.dir.join(PROMPTS_DIR));
        (!copies.holds(&format!("{id}.md"), &file.text)).then_some(file)
    }

    fn read(&self, relative: &str) -> Option<YourFile> {
        let path = self.dir.join(relative);
        if self.waiting.is_some_and(|waiting| waiting.contains(&path)) {
            return None;
        }
        let text = std::fs::read_to_string(&path).ok()?;
        Some(YourFile {
            text,
            path,
            shown: format!("{}{relative}", self.prefix),
        })
    }
}

/// The first of `names` that `text` says as a word of its own, whatever its case. A file for
/// the reviewers or the judge that names a model or a CLI tells them who made the work, which
/// they aren't told otherwise.
pub fn names_one_of<'a>(text: &str, names: &'a [String]) -> Option<&'a str> {
    let lower = text.to_lowercase();
    names
        .iter()
        .map(String::as_str)
        .filter(|name| !name.trim().is_empty())
        .find(|name| {
            let name = name.to_lowercase();
            lower.match_indices(&name).any(|(at, _)| {
                let before = lower[..at].chars().next_back();
                let after = lower[at + name.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
        })
}

/// Splits a role file into its front matter and its text. The front matter is optional and
/// holds one key, `mode: add` or `mode: replace`. Anything else in it is refused by name, so a
/// typo never silently changes what a role is told. A file with no text yet, such as the
/// starter before you wrote in it, is no role file.
pub fn parse_role_file(file: YourFile) -> Result<Option<RoleFile>, String> {
    let YourFile { text, path, shown } = file;
    let (front, body) = front_matter(&text);
    let mut mode = RoleFileMode::default();
    for line in front.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .map(|(key, value)| (key.trim(), value.trim().trim_matches(['"', '\''])))
            .ok_or_else(|| format!("{shown}: `{line}` isn't `key: value`"))?;
        mode = match (key, value) {
            ("mode", "add") => RoleFileMode::Add,
            ("mode", "replace") => RoleFileMode::Replace,
            ("mode", other) => {
                return Err(format!(
                    "{shown}: mode is `add` or `replace`, not `{other}`"
                ));
            }
            (other, _) => {
                return Err(format!(
                    "{shown}: `{other}` isn't a key a role file takes; only `mode` is"
                ));
            }
        };
    }
    let text = body.trim().to_owned();
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(RoleFile {
        mode,
        text,
        shown,
        path,
    }))
}

/// Writes a starter `roles/<role>.md` into `argonauts_dir`, with nothing in it but how it is
/// used, so it changes nothing until you write in it. `Ok(false)` when your file was there
/// and was kept.
pub fn write_role_file(argonauts_dir: &Path, readers: impl Into<Readers>) -> io::Result<bool> {
    let readers = readers.into();
    let path = argonauts_dir
        .join(ROLES_DIR)
        .join(format!("{}.md", readers.as_str()));
    write_new(&path, &role_file_starter(readers))
}

/// Saves `text` as your template for one prompt, in place of any you had, and says where.
/// For when the main session writes the change you asked it for.
pub fn save_template(argonauts_dir: &Path, template: &Template, text: &str) -> io::Result<PathBuf> {
    let path = argonauts_dir
        .join(PROMPTS_DIR)
        .join(format!("{}.md", template.id));
    save(&path, text)?;
    Ok(path)
}

/// Saves `text` as your instructions for a role, in place of any you had, and says where.
pub fn save_role_file(
    argonauts_dir: &Path,
    readers: impl Into<Readers>,
    mode: RoleFileMode,
    text: &str,
) -> io::Result<PathBuf> {
    let path = argonauts_dir
        .join(ROLES_DIR)
        .join(format!("{}.md", readers.into().as_str()));
    let mode = match mode {
        RoleFileMode::Add => "add",
        RoleFileMode::Replace => "replace",
    };
    save(&path, &format!("---\nmode: {mode}\n---\n{}\n", text.trim()))?;
    Ok(path)
}

fn save(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

/// The starter's text: the front matter alone, which says what the file is for.
pub fn role_file_starter(readers: impl Into<Readers>) -> String {
    let readers = readers.into();
    let who = readers.the_agents();
    // A finer role's file is read in place of its crew role's, which is easy to miss.
    let alone = match readers {
        Readers::Crew(_) => String::new(),
        Readers::Finer(_) => format!(
            "# While this file says something, {who} reads it in place of {ROLES_DIR}/{}.md.\n",
            readers.crew().as_str()
        ),
    };
    format!(
        "---\n# Your own instructions for {who}. Write them under the second line of \
         dashes.\n# add puts them after the instructions {who} would get anyway. replace takes \
         those instructions' place.\n{alone}mode: add\n---\n"
    )
}

/// The lines between a leading `---` and the next `---`, and everything after. A file that
/// doesn't open with `---` has no front matter.
fn front_matter(text: &str) -> (&str, &str) {
    let Some(rest) = text.trim_start().strip_prefix("---") else {
        return ("", text);
    };
    let Some(rest) = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
    else {
        return ("", text);
    };
    match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + "\n---".len()..];
            let body = after.split_once('\n').map_or("", |(_, body)| body);
            (&rest[..end], body)
        }
        None => ("", text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(project: &Path, home: &Path) -> PromptDirs {
        PromptDirs::new(Some(project), Some(home))
    }

    fn write(path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn the_projects_template_wins_and_deleting_it_goes_back() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let dirs = dirs(repo.path(), home.path());
        assert_eq!(dirs.template("review/review"), None);

        write(
            home.path().join("prompts/review/review.md"),
            "Mine everywhere.",
        );
        let found = dirs.template("review/review").unwrap();
        assert_eq!(found.text, "Mine everywhere.");
        assert_eq!(found.shown, "~/.argonauts/prompts/review/review.md");

        let project = repo.path().join(".argonauts/prompts/review/review.md");
        write(project.clone(), "This project's.");
        let found = dirs.template("review/review").unwrap();
        assert_eq!(found.text, "This project's.");
        assert_eq!(found.shown, "prompts/review/review.md");

        std::fs::remove_file(project).unwrap();
        assert_eq!(
            dirs.template("review/review").unwrap().text,
            "Mine everywhere."
        );
    }

    #[test]
    fn templates_are_written_once_and_yours_are_kept() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".argonauts");
        let templates = [
            Template {
                id: "frames/reviewer",
                role: CrewRole::Reviewer,
                part: "frame",
                text: "You review.\n",
            },
            Template {
                id: "review/review",
                role: CrewRole::Reviewer,
                part: "review a change",
                text: "Review the change.\n",
            },
        ];
        write(dir.join("prompts/review/review.md"), "Mine.\n");
        let done = write_templates(&dir, &templates).unwrap();
        assert_eq!(done.written, ["frames/reviewer.md"]);
        assert_eq!(done.kept, ["review/review.md"]);
        let read = |file: &str| std::fs::read_to_string(dir.join("prompts").join(file)).unwrap();
        assert_eq!(read("frames/reviewer.md"), "You review.\n");
        assert_eq!(read("review/review.md"), "Mine.\n");
        let readme = read("README.md");
        assert!(readme.contains("Nothing is locked"), "{readme}");
        assert!(
            readme.contains("- `review/review.md`: review a change"),
            "{readme}"
        );
        // What was written is a copy, not a template of yours, and yours still is one.
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert_eq!(dirs.template("frames/reviewer"), None);
        assert_eq!(
            dirs.template_file("frames/reviewer").unwrap().text,
            "You review.\n"
        );
        assert_eq!(dirs.template("review/review").unwrap().text, "Mine.\n");
    }

    fn frame(text: &'static str) -> [Template; 1] {
        [Template {
            id: "frames/reviewer",
            role: CrewRole::Reviewer,
            part: "frame",
            text,
        }]
    }

    /// `/setup` writes every template out. A later version that words one better must reach
    /// whoever never touched the file, and must leave alone whoever did.
    #[test]
    fn a_copy_nobody_edited_follows_the_built_in_and_an_edited_one_is_yours() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".argonauts");
        let file = dir.join("prompts/frames/reviewer.md");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let read = |path: &Path| std::fs::read_to_string(path).unwrap();

        let first = write_templates(&dir, &frame("You review.\n")).unwrap();
        assert_eq!(first.written, ["frames/reviewer.md"]);
        let again = write_templates(&dir, &frame("You review.\n")).unwrap();
        assert_eq!((again.written.len(), again.unchanged), (0, 1));

        // The built-in changed and the copy didn't: the copy is passed over, then caught up.
        assert_eq!(dirs.template("frames/reviewer"), None);
        let newer = write_templates(&dir, &frame("You review one change.\n")).unwrap();
        assert_eq!(newer.written, ["frames/reviewer.md"]);
        assert_eq!(read(&file), "You review one change.\n");
        assert!(read(&dir.join("prompts/README.md")).contains("Nothing is locked"));

        write(file.clone(), "You review, strictly.\n");
        assert_eq!(
            dirs.template("frames/reviewer").unwrap().text,
            "You review, strictly.\n"
        );
        let newest = write_templates(&dir, &frame("You review a change.\n")).unwrap();
        assert_eq!(newest.kept, ["frames/reviewer.md"]);
        assert_eq!(read(&file), "You review, strictly.\n");
        assert_eq!(
            write_template(&dir, &frame("You review a change.\n")[0]).unwrap(),
            Copied::Yours
        );
    }

    /// A file that reads as the built-in does, written by hand or by a version that kept no
    /// record, is taken for the copy it is.
    #[test]
    fn a_file_that_matches_the_built_in_becomes_a_copy() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".argonauts");
        write(dir.join("prompts/frames/reviewer.md"), "You review.\n");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert!(dirs.template("frames/reviewer").is_some());
        assert_eq!(
            write_template(&dir, &frame("You review.\n")[0]).unwrap(),
            Copied::AsBuiltIn
        );
        assert_eq!(dirs.template("frames/reviewer"), None);
    }

    /// Copies are never read by an agent, so a repository that brings them asks for no trust.
    #[test]
    fn a_copy_nobody_edited_isnt_a_file_an_agent_may_read() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".argonauts");
        write_templates(&dir, &frame("You review.\n")).unwrap();
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert_eq!(dirs.project_files(), Vec::<PathBuf>::new());
        let file = dir.join("prompts/frames/reviewer.md");
        write(file.clone(), "You review, and you go easy.\n");
        assert_eq!(dirs.project_files(), [file]);
    }

    #[test]
    fn a_file_that_waits_for_trust_is_passed_over() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let theirs = repo.path().join(".argonauts/roles/reviewer.md");
        write(theirs.clone(), "Go easy on this code.");
        write(repo.path().join(".argonauts/prompts/README.md"), "For you.");
        write(home.path().join("roles/reviewer.md"), "Mine everywhere.");
        let all = dirs(repo.path(), home.path());
        assert_eq!(all.project_files(), std::slice::from_ref(&theirs));
        let role = all.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(role.text, "Go easy on this code.");

        let careful = all.without([theirs]);
        let role = careful.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(role.text, "Mine everywhere.");
    }

    #[test]
    fn what_you_asked_for_is_saved_where_the_lookup_finds_it() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".argonauts");
        let template = Template {
            id: "frames/reviewer",
            role: CrewRole::Reviewer,
            part: "frame",
            text: "You review.\n",
        };
        let saved = save_template(&dir, &template, "You review, strictly.\n").unwrap();
        assert!(saved.ends_with(".argonauts/prompts/frames/reviewer.md"));
        save_role_file(
            &dir,
            CrewRole::Reviewer,
            RoleFileMode::Replace,
            "A missing test is a required change.\n\n",
        )
        .unwrap();
        // A second save takes the first one's place.
        save_role_file(
            &dir,
            CrewRole::Reviewer,
            RoleFileMode::Add,
            "A missing test is a required change.",
        )
        .unwrap();

        let dirs = PromptDirs::new(Some(repo.path()), None);
        let frame = dirs.template("frames/reviewer").unwrap();
        assert_eq!(frame.text, "You review, strictly.\n");
        let role = dirs.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(role.mode, RoleFileMode::Add);
        assert_eq!(role.text, "A missing test is a required change.");
    }

    /// The one test that sets this process's folders, since they are set once for all.
    #[test]
    fn a_program_looks_for_your_template_each_time_it_sends_a_prompt() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_owned();
        use_dirs_from(move || PromptDirs::new(Some(&root), None));
        assert_eq!(in_use("conversation/answer", "Built in."), "Built in.");
        let file = repo
            .path()
            .join(".argonauts/prompts/conversation/answer.md");
        write(file.clone(), "Mine.");
        assert_eq!(in_use("conversation/answer", "Built in."), "Mine.");
        std::fs::remove_file(file).unwrap();
        assert_eq!(in_use("conversation/answer", "Built in."), "Built in.");
    }

    #[test]
    fn a_starter_role_file_changes_nothing_until_you_write_in_it() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".argonauts");
        assert!(write_role_file(&dir, CrewRole::Reviewer).unwrap());
        let file = dir.join("roles/reviewer.md");
        let starter = std::fs::read_to_string(&file).unwrap();
        assert!(
            starter.contains("# Your own instructions for the reviewers."),
            "{starter}"
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert_eq!(dirs.role(RoleName::Reviewer), None);
        assert_eq!(dirs.own_role(CrewRole::Reviewer), None);

        // What you write under the dashes is the role's file, and a second starter keeps it.
        std::fs::write(&file, format!("{starter}Check error handling.\n")).unwrap();
        assert!(!write_role_file(&dir, CrewRole::Reviewer).unwrap());
        let role = dirs.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(role.mode, RoleFileMode::Add);
        assert_eq!(role.text, "Check error handling.");
    }

    /// A finer role's file goes by its own name, says whose file it replaces, and is
    /// saved and found apart from its crew role's.
    #[test]
    fn a_finer_role_has_a_file_under_its_own_name() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".argonauts");
        let critic = Readers::Finer(RoleName::Critic);
        assert!(write_role_file(&dir, critic).unwrap());
        let starter = std::fs::read_to_string(dir.join("roles/critic.md")).unwrap();
        assert!(
            starter.contains("the critic role reads it in place of roles/reviewer.md."),
            "{starter}"
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert_eq!(dirs.own_role_file(critic), None, "a starter says nothing");

        save_role_file(
            &dir,
            CrewRole::Reviewer,
            RoleFileMode::Add,
            "All reviewers.",
        )
        .unwrap();
        let saved = save_role_file(&dir, critic, RoleFileMode::Replace, "Critics only.").unwrap();
        assert_eq!(saved, dir.join("roles/critic.md"));
        let own = dirs.own_role(critic).unwrap().unwrap();
        assert_eq!(
            (own.mode, own.text.as_str()),
            (RoleFileMode::Replace, "Critics only.")
        );
        let crew = dirs.own_role(CrewRole::Reviewer).unwrap().unwrap();
        assert_eq!(crew.text, "All reviewers.");
        assert_eq!(dirs.own_role(Readers::Finer(RoleName::Consultant)), None);
    }

    #[test]
    fn a_file_that_names_an_agent_is_noticed_by_whole_words() {
        let names = ["Codex".to_owned(), "Opus 5.5".to_owned(), "agy".to_owned()];
        assert_eq!(
            names_one_of("Go easy on codex's code.", &names),
            Some("Codex")
        );
        assert_eq!(
            names_one_of("Trust opus 5.5 less.", &names),
            Some("Opus 5.5")
        );
        // `agy` inside another word isn't the CLI.
        assert_eq!(
            names_one_of("Check the strategy and the codexes.", &names),
            None
        );
        assert_eq!(names_one_of("Check error handling.", &names), None);
    }

    #[test]
    fn a_blank_template_is_no_template() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path().join(".argonauts/prompts/plan/draft.md"), " \n");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert_eq!(dirs.template("plan/draft"), None);
    }

    #[test]
    fn a_role_file_adds_unless_it_says_replace() {
        let repo = tempfile::tempdir().unwrap();
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let file = repo.path().join(".argonauts/roles/reviewer.md");
        write(
            file.clone(),
            "---\nmode: add            # add (default) · replace\n---\nTreat a missing test as a required change.\n",
        );
        let role = dirs.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(role.mode, RoleFileMode::Add);
        assert_eq!(role.text, "Treat a missing test as a required change.");
        assert_eq!(role.shown, "roles/reviewer.md");
        // The critic follows the reviewers, so it reads their file too.
        assert!(dirs.role(RoleName::Critic).unwrap().is_ok());
        assert_eq!(dirs.role(RoleName::Worker), None);

        write(file.clone(), "---\nmode: replace\n---\nOnly this.\n");
        let role = dirs.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(role.mode, RoleFileMode::Replace);

        write(file, "No front matter at all.\n");
        let role = dirs.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(role.mode, RoleFileMode::Add);
        assert_eq!(role.text, "No front matter at all.");
    }

    #[test]
    fn a_roles_own_file_comes_before_its_crew_roles() {
        let repo = tempfile::tempdir().unwrap();
        let dirs = PromptDirs::new(Some(repo.path()), None);
        write(
            repo.path().join(".argonauts/roles/reviewer.md"),
            "All reviewers.",
        );
        write(
            repo.path().join(".argonauts/roles/critic.md"),
            "Critics only.",
        );
        let critic = dirs.role(RoleName::Critic).unwrap().unwrap();
        assert_eq!(critic.text, "Critics only.");
        let reviewer = dirs.role(RoleName::Reviewer).unwrap().unwrap();
        assert_eq!(reviewer.text, "All reviewers.");
    }

    #[test]
    fn a_role_file_with_a_wrong_key_says_which() {
        let repo = tempfile::tempdir().unwrap();
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let file = repo.path().join(".argonauts/roles/worker.md");
        write(file.clone(), "---\nmodel: opus\n---\nBuild carefully.\n");
        let err = dirs.role(RoleName::Worker).unwrap().unwrap_err();
        assert!(err.contains("`model` isn't a key"), "{err}");
        write(file.clone(), "---\nmode: append\n---\nBuild carefully.\n");
        let err = dirs.role(RoleName::Worker).unwrap().unwrap_err();
        assert!(err.contains("not `append`"), "{err}");
        // A file with no text yet is no role file, so nothing is said about it.
        write(file, "---\nmode: add\n---\n\n");
        assert_eq!(dirs.role(RoleName::Worker), None);
    }
}
