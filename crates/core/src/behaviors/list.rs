//! The fixed list of behavior labels. Each label keeps its id, its words and its place for as
//! long as [`LIST_VERSION`] stands: usage reports send the ids, and the public map draws the
//! counts at these places, so a change here is a new version.

use serde::{Deserialize, Serialize};

/// Raised whenever a label is added, removed, reworded or moved.
pub const LIST_VERSION: u32 = 1;

/// A group of labels that sit together on the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Reading,
    Searching,
    Listing,
    Tests,
    Building,
    GitHistory,
    GitStatus,
    GitChanges,
    Shell,
    SourceEdits,
    TestEdits,
    NewFiles,
    ConfigEdits,
    DocEdits,
    Installs,
    Web,
    Answers,
    Asking,
    Workflows,
    Planning,
    Records,
}

/// One label of the list. Its serde name is its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    ReadSource,
    ReadTest,
    ReadConfig,
    ReadDoc,
    ReadFailed,
    PrintFiles,
    PrintTestFiles,
    SearchCode,
    ListFiles,
    RunTests,
    RunTestsFailed,
    Build,
    Lint,
    GitHistory,
    GitStatus,
    GitChange,
    Shell,
    PythonSnippet,
    FileOps,
    EditSource,
    EditFailed,
    DeleteFile,
    EditTest,
    EditTestAdded,
    EditTestAssertion,
    EditTestRemoved,
    WriteSource,
    WriteTest,
    WriteConfig,
    WriteDoc,
    EditConfig,
    EditDoc,
    Install,
    WebFetch,
    WebSearch,
    Answer,
    AnswerFailed,
    AskReview,
    Subagent,
    AskUser,
    Workflow,
    Todo,
    PresentPlan,
    ToolSearch,
    McpTool,
    ReadRecords,
    ListRecords,
    RunStatus,
}

/// What the list says of one label.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelInfo {
    pub label: Label,
    pub id: &'static str,
    pub words: &'static str,
    pub area: Area,
    /// Where it sits on the map, each in 0..1: `x` from the left, `y` from the top.
    pub place: (f32, f32),
}

const fn info(
    label: Label,
    id: &'static str,
    words: &'static str,
    area: Area,
    place: (f32, f32),
) -> LabelInfo {
    LabelInfo {
        label,
        id,
        words,
        area,
        place,
    }
}

use Area as A;
use Label as L;

/// Every label, in the order of [`Label`]'s variants. Reading sits top left, tests top
/// middle, git top right, the shell's other commands in the middle, edits along the bottom,
/// answers and asking bottom right, and Argonauts' own records bottom left.
#[rustfmt::skip]
pub const LABELS: [LabelInfo; 48] = [
    info(L::ReadSource, "read_source", "read a source file", A::Reading, (0.10, 0.14)),
    info(L::ReadTest, "read_test", "read a test file", A::Reading, (0.17, 0.09)),
    info(L::ReadConfig, "read_config", "read a config file", A::Reading, (0.05, 0.21)),
    info(L::ReadDoc, "read_doc", "read a doc", A::Reading, (0.12, 0.26)),
    info(L::ReadFailed, "read_failed", "failed to read a file", A::Reading, (0.04, 0.31)),
    info(L::PrintFiles, "print_files", "printed files", A::Reading, (0.21, 0.17)),
    info(L::PrintTestFiles, "print_test_files", "printed test files", A::Reading, (0.25, 0.09)),
    info(L::SearchCode, "search_code", "searched code by pattern", A::Searching, (0.12, 0.43)),
    info(L::ListFiles, "list_files", "listed files", A::Listing, (0.22, 0.51)),
    info(L::RunTests, "run_tests", "ran the test suite", A::Tests, (0.43, 0.09)),
    info(L::RunTestsFailed, "run_tests_failed", "ran the test suite, which failed", A::Tests, (0.51, 0.13)),
    info(L::Build, "build", "built the project", A::Building, (0.38, 0.23)),
    info(L::Lint, "lint", "ran a linter or formatter", A::Building, (0.47, 0.25)),
    info(L::GitHistory, "git_history", "read git history", A::GitHistory, (0.81, 0.09)),
    info(L::GitStatus, "git_status", "checked git status or diff", A::GitStatus, (0.73, 0.17)),
    info(L::GitChange, "git_change", "changed git state", A::GitChanges, (0.87, 0.25)),
    info(L::Shell, "shell", "ran a shell command", A::Shell, (0.53, 0.42)),
    info(L::PythonSnippet, "python_snippet", "ran a Python snippet", A::Shell, (0.60, 0.46)),
    info(L::FileOps, "file_ops", "moved, copied or removed files", A::Shell, (0.46, 0.46)),
    info(L::EditSource, "edit_source", "edited a source file", A::SourceEdits, (0.45, 0.68)),
    info(L::EditFailed, "edit_failed", "tried an edit that didn't apply", A::SourceEdits, (0.49, 0.80)),
    info(L::DeleteFile, "delete_file", "deleted a file", A::SourceEdits, (0.50, 0.73)),
    info(L::EditTest, "edit_test", "edited a test file", A::TestEdits, (0.30, 0.66)),
    info(L::EditTestAdded, "edit_test_added", "edited a test file: added tests", A::TestEdits, (0.25, 0.72)),
    info(L::EditTestAssertion, "edit_test_assertion", "edited a test file: changed an assertion", A::TestEdits, (0.33, 0.74)),
    info(L::EditTestRemoved, "edit_test_removed", "edited a test file: removed or skipped a test", A::TestEdits, (0.28, 0.81)),
    info(L::WriteSource, "write_source", "wrote a source file", A::NewFiles, (0.55, 0.62)),
    info(L::WriteTest, "write_test", "wrote a test file", A::NewFiles, (0.48, 0.60)),
    info(L::WriteConfig, "write_config", "wrote a config file", A::NewFiles, (0.63, 0.58)),
    info(L::WriteDoc, "write_doc", "wrote a doc", A::NewFiles, (0.70, 0.62)),
    info(L::EditConfig, "edit_config", "edited a config file", A::ConfigEdits, (0.63, 0.70)),
    info(L::EditDoc, "edit_doc", "edited a doc", A::DocEdits, (0.68, 0.78)),
    info(L::Install, "install", "installed a dependency", A::Installs, (0.84, 0.42)),
    info(L::WebFetch, "web_fetch", "fetched from the web", A::Web, (0.93, 0.49)),
    info(L::WebSearch, "web_search", "searched the web", A::Web, (0.91, 0.57)),
    info(L::Answer, "answer", "returned its structured answer", A::Answers, (0.84, 0.73)),
    info(L::AnswerFailed, "answer_failed", "gave a structured answer that didn't fit", A::Answers, (0.91, 0.79)),
    info(L::AskReview, "ask_review", "asked another agent to review or judge", A::Asking, (0.76, 0.87)),
    info(L::Subagent, "subagent", "started a subagent", A::Asking, (0.82, 0.93)),
    info(L::AskUser, "ask_user", "asked you a question", A::Asking, (0.70, 0.93)),
    info(L::Workflow, "workflow", "wrote a workflow script", A::Workflows, (0.93, 0.91)),
    info(L::Todo, "todo", "updated its to-do list", A::Planning, (0.37, 0.92)),
    info(L::PresentPlan, "present_plan", "presented a plan", A::Planning, (0.30, 0.95)),
    info(L::ToolSearch, "tool_search", "looked up a deferred tool", A::Planning, (0.45, 0.94)),
    info(L::McpTool, "mcp_tool", "used an MCP tool", A::Planning, (0.56, 0.91)),
    info(L::ReadRecords, "read_records", "read Argonauts' own run records", A::Records, (0.07, 0.72)),
    info(L::ListRecords, "list_records", "listed Argonauts' own run records", A::Records, (0.13, 0.78)),
    info(L::RunStatus, "run_status", "checked the run's status", A::Records, (0.06, 0.86)),
];

impl Label {
    pub fn info(self) -> &'static LabelInfo {
        &LABELS[self as usize]
    }

    pub fn id(self) -> &'static str {
        self.info().id
    }

    pub fn words(self) -> &'static str {
        self.info().words
    }

    pub fn area(self) -> Area {
        self.info().area
    }

    pub fn place(self) -> (f32, f32) {
        self.info().place
    }

    pub fn from_id(id: &str) -> Option<Self> {
        LABELS
            .iter()
            .find(|info| info.id == id)
            .map(|info| info.label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_follows_the_variants() {
        for (at, info) in LABELS.iter().enumerate() {
            assert_eq!(info.label as usize, at, "{} is out of order", info.id);
            let id = serde_json::to_value(info.label).unwrap();
            assert_eq!(id, info.id, "the id is the label's serde name");
            assert_eq!(Label::from_id(info.id), Some(info.label));
        }
    }

    #[test]
    fn ids_and_words_are_unique() {
        for (at, a) in LABELS.iter().enumerate() {
            for b in &LABELS[at + 1..] {
                assert_ne!(a.id, b.id);
                assert_ne!(a.words, b.words);
            }
        }
    }

    /// Two labels at one place would draw as one cloud, and the public map could not tell
    /// them apart.
    #[test]
    fn every_place_is_distinct_and_on_the_map() {
        for (at, a) in LABELS.iter().enumerate() {
            let (x, y) = a.place;
            assert!(
                (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y),
                "{}",
                a.id
            );
            for b in &LABELS[at + 1..] {
                assert!(
                    distance(a.place, b.place) >= 0.05,
                    "{} and {} are too close",
                    a.id,
                    b.id
                );
            }
        }
    }

    /// A label's nearest neighbour is one of its own area, wherever the area has more
    /// than one label, so each area reads as one region of the map.
    #[test]
    fn each_area_sits_together() {
        for a in &LABELS {
            let kin = LABELS
                .iter()
                .filter(|b| b.area == a.area && b.id != a.id)
                .count();
            if kin == 0 {
                continue;
            }
            let nearest = LABELS
                .iter()
                .filter(|b| b.id != a.id)
                .min_by(|b, c| distance(a.place, b.place).total_cmp(&distance(a.place, c.place)))
                .unwrap();
            assert_eq!(
                nearest.area, a.area,
                "{} sits nearest {}, of another area",
                a.id, nearest.id
            );
        }
    }

    #[test]
    fn the_list_is_the_size_the_map_is_drawn_for() {
        assert!((40..=60).contains(&LABELS.len()));
    }

    fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
    }
}
