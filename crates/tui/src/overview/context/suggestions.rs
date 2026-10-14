//! What Claude Code's /context suggests, by its own nine rules and in its own words, with
//! what each would save. The rules read the same numbers its reply carries.

use argonauts_core::events::context::{ContextUsage, ToolTokens};
use ratatui::text::Span;

use super::messages::mark;
use super::place::{Columns, at, ending, place};
use super::usage::{bold, level, level_role};
use super::words::whole_percent;
use crate::bridge::tools::Places;
use crate::format;
use crate::kit::{Drawn, Kit};
use crate::text::{spans_width, str_width};
use crate::theme::Role;

/// A tool is named from this share of the window.
const TOOL_PERCENT: f64 = 15.0;
/// A tool no rule knows is named from this share.
const OTHER_TOOL_PERCENT: f64 = 20.0;
/// What a tool or the file reads must hold at least to be named.
const TOOL_TOKENS: u64 = 10_000;
const FILE_READS_PERCENT: f64 = 5.0;
const MEMORY_PERCENT: f64 = 5.0;
const MEMORY_TOKENS: u64 = 5_000;
/// The context is close to full from this percent.
const NEAR_FULL: u32 = 80;
/// Without auto-compact, a context this full is worth a word.
const UNCOMPACTED_FROM: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Severity {
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Suggestion {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// What following it would free, by Claude Code's estimate.
    pub savings: Option<u64>,
    /// The tool it is about, as the reply names it.
    pub tool: Option<String>,
    /// It says the context is nearly full, so it reads first and has no saving.
    pub near_full: bool,
}

/// Every suggestion the count calls for: the one about a nearly full context first, then
/// warnings before notes, the largest saving first. `no_compacting` is `DISABLE_COMPACT`, which
/// turns compacting off whole.
pub(super) fn suggestions(
    usage: &ContextUsage,
    places: &Places,
    no_compacting: bool,
) -> Vec<Suggestion> {
    let mut found = Vec::new();
    found.extend(near_full(usage, no_compacting));
    found.extend(tools(usage));
    found.extend(file_reads(usage));
    found.extend(memory(usage, places));
    found.extend(uncompacted(usage, no_compacting));
    found.sort_by_key(|suggestion| {
        (
            !suggestion.near_full,
            suggestion.severity == Severity::Note,
            std::cmp::Reverse(suggestion.savings.unwrap_or(0)),
        )
    });
    found
}

/// Whether the environment turns compacting off, as Claude Code reads `DISABLE_COMPACT`.
pub(super) fn compacting_disabled() -> bool {
    std::env::var("DISABLE_COMPACT").is_ok_and(|value| {
        matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn percent_of_window(tokens: u64, usage: &ContextUsage) -> f64 {
    if usage.raw_window == 0 {
        return 0.0;
    }
    tokens as f64 / usage.raw_window as f64 * 100.0
}

fn near_full(usage: &ContextUsage, no_compacting: bool) -> Option<Suggestion> {
    if usage.total_tokens > usage.raw_window || usage.percentage < NEAR_FULL {
        return None;
    }
    let detail = if usage.auto_compact {
        "Autocompact will trigger soon, which discards older messages. Use /compact now to \
         control what gets kept."
    } else if no_compacting {
        "Compaction is disabled."
    } else {
        "Autocompact is disabled. Use /compact to free space, or enable autocompact in /config."
    };
    Some(Suggestion {
        severity: Severity::Warning,
        title: format!("Context is {}% full", usage.percentage),
        detail: detail.to_owned(),
        savings: None,
        tool: None,
        near_full: true,
    })
}

fn tools(usage: &ContextUsage) -> Vec<Suggestion> {
    let Some(messages) = &usage.messages else {
        return Vec::new();
    };
    messages
        .tools
        .iter()
        .filter(|tool| {
            percent_of_window(tool.total(), usage) >= TOOL_PERCENT && tool.total() >= TOOL_TOKENS
        })
        .filter_map(|tool| about_tool(tool, usage))
        .collect()
}

/// The rule for one tool over the line: Claude Code knows five by name, and names any other
/// only from a fifth of the window.
fn about_tool(tool: &ToolTokens, usage: &ContextUsage) -> Option<Suggestion> {
    let tokens = tool.total();
    let name = tool.name.as_str();
    let (severity, detail, saved) = match name {
        "Bash" => (
            Severity::Warning,
            "Pipe output through head, tail, or grep to reduce result size. Avoid cat on large \
             files \u{2014} use Read with offset/limit instead.",
            0.5,
        ),
        "PowerShell" => (
            Severity::Warning,
            "Pipe output through Select-Object -First/-Last or Select-String to reduce result \
             size. Avoid Get-Content on large files \u{2014} use Read with offset/limit instead.",
            0.5,
        ),
        "Read" => (
            Severity::Note,
            "Use offset and limit parameters to read only the sections you need. Avoid \
             re-reading entire files when you only need a few lines.",
            0.3,
        ),
        "Grep" => (
            Severity::Note,
            "Add more specific patterns or use the glob or type parameter to narrow file types. \
             Consider Glob for file discovery instead of Grep.",
            0.3,
        ),
        "WebFetch" => (
            Severity::Note,
            "Web page content can be very large. Consider extracting only the specific \
             information needed.",
            0.4,
        ),
        _ if percent_of_window(tokens, usage) >= OTHER_TOOL_PERCENT => (
            Severity::Note,
            "This tool is consuming a significant portion of context.",
            0.2,
        ),
        _ => return None,
    };
    let known = matches!(name, "Bash" | "PowerShell" | "Read" | "Grep" | "WebFetch");
    let results = if known { " results" } else { "" };
    Some(Suggestion {
        severity,
        title: format!(
            "{name}{results} using {} tokens ({}%)",
            format::tokens(tokens),
            whole_percent(tokens, usage.raw_window)
        ),
        detail: detail.to_owned(),
        savings: Some((tokens as f64 * saved).floor() as u64),
        tool: Some(tool.name.clone()),
        near_full: false,
    })
}

/// Read's results alone, where its calls and results together stay under the tools' line.
fn file_reads(usage: &ContextUsage) -> Option<Suggestion> {
    let read = usage
        .messages
        .as_ref()?
        .tools
        .iter()
        .find(|tool| tool.name == "Read")?;
    let named_already =
        percent_of_window(read.total(), usage) >= TOOL_PERCENT && read.total() >= TOOL_TOKENS;
    let results = read.result_tokens;
    if named_already
        || percent_of_window(results, usage) < FILE_READS_PERCENT
        || results < TOOL_TOKENS
    {
        return None;
    }
    Some(Suggestion {
        severity: Severity::Note,
        title: format!(
            "File reads using {} tokens ({}%)",
            format::tokens(results),
            whole_percent(results, usage.raw_window)
        ),
        detail: "If you are re-reading files, consider referencing earlier reads. Use \
                 offset/limit for large files."
            .to_owned(),
        savings: Some((results as f64 * 0.3).floor() as u64),
        tool: Some(read.name.clone()),
        near_full: false,
    })
}

fn memory(usage: &ContextUsage, places: &Places) -> Option<Suggestion> {
    let tokens: u64 = usage.memory_files.iter().map(|file| file.tokens).sum();
    if percent_of_window(tokens, usage) < MEMORY_PERCENT || tokens < MEMORY_TOKENS {
        return None;
    }
    let mut files: Vec<_> = usage.memory_files.iter().collect();
    files.sort_by_key(|file| std::cmp::Reverse(file.tokens));
    let largest: Vec<String> = files
        .iter()
        .take(3)
        .map(|file| {
            format!(
                "{} ({})",
                places.show(&file.path),
                format::tokens(file.tokens)
            )
        })
        .collect();
    Some(Suggestion {
        severity: Severity::Note,
        title: format!(
            "Memory files using {} tokens ({}%)",
            format::tokens(tokens),
            whole_percent(tokens, usage.raw_window)
        ),
        detail: format!(
            "Largest: {}. Use /memory to review and prune stale entries.",
            largest.join(", ")
        ),
        savings: Some((tokens as f64 * 0.3).floor() as u64),
        tool: None,
        near_full: false,
    })
}

fn uncompacted(usage: &ContextUsage, no_compacting: bool) -> Option<Suggestion> {
    let worth_a_word = (UNCOMPACTED_FROM..NEAR_FULL).contains(&usage.percentage);
    if usage.auto_compact || no_compacting || !worth_a_word {
        return None;
    }
    Some(Suggestion {
        severity: Severity::Note,
        title: "Autocompact is disabled".to_owned(),
        detail: "Without autocompact, you will hit context limits and lose the conversation. \
                 Enable it in /config or use /compact manually."
            .to_owned(),
        savings: None,
        tool: None,
        near_full: false,
    })
}

/// `75% → 61%`: the window's fill now, and after `saved` tokens are gone.
fn fill_after(usage: &ContextUsage, saved: u64) -> String {
    format!(
        "{}% \u{2192} {}%",
        usage.percentage,
        whole_percent(usage.total_tokens.saturating_sub(saved), usage.raw_window)
    )
}

/// `text` in rows of at most `cells`, broken between words.
fn wrapped(text: &str, cells: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match rows.last_mut() {
            Some(row) if str_width(row) + 1 + str_width(word) <= cells => {
                row.push(' ');
                row.push_str(word);
            }
            _ => rows.push(word.to_owned()),
        }
    }
    rows
}

/// The suggestions: a header that says what they would save together, then a block each,
/// with its mark, Claude Code's title and detail, and flush right what it would save and how
/// full the window would be then.
pub(super) fn blocks(
    kit: &Kit<'_>,
    columns: &Columns,
    usage: &ContextUsage,
    found: &[Suggestion],
) -> Vec<Drawn> {
    if found.is_empty() {
        return Vec::new();
    }
    let indent = columns.indent();
    let savings: Vec<u64> = found.iter().filter_map(|found| found.savings).collect();
    let together = if savings.len() > 1 {
        let all: u64 = savings.iter().sum();
        vec![kit.dim(format!(
            "together: save ~{} \u{b7} {}",
            format::tokens(all),
            fill_after(usage, all)
        ))]
    } else {
        Vec::new()
    };
    let mut header = Drawn::new();
    header.push(place([
        at(indent, vec![bold(kit, "Suggestions")]),
        ending(columns.edge, together),
    ]));
    let mut blocks = vec![header];
    for suggestion in found {
        let mut drawn = Drawn::new();
        let title = vec![
            mark(kit, suggestion.severity),
            bold(kit, suggestion.title.clone()),
        ];
        let gain = match suggestion.savings {
            Some(saved) => vec![
                bold(kit, format!("save ~{}", format::tokens(saved))),
                kit.dim(format!("   {}", fill_after(usage, saved))),
            ],
            None if suggestion.near_full => {
                let left = format::tokens(usage.limit().saturating_sub(usage.total_tokens));
                let words = match usage.compacts_at() {
                    Some(_) => format!("compacts in {left}"),
                    None => format!("{left} left"),
                };
                vec![kit.span(words, level_role(level(usage)).unwrap_or(Role::Warn))]
            }
            None => Vec::new(),
        };
        // On a narrow panel the saving goes under the title, never over it.
        if indent + spans_width(&title) + 2 + spans_width(&gain) <= columns.edge {
            drawn.push(place([at(indent, title), ending(columns.edge, gain)]));
        } else {
            drawn.push(place([at(indent, title)]));
            drawn.push(place([at(indent + 2, gain)]));
        }
        let room = columns.edge.saturating_sub(indent + 2).max(20);
        for row in wrapped(&suggestion.detail, room) {
            drawn.push(vec![Span::raw(" ".repeat(indent + 2)), kit.dim(row)]);
        }
        blocks.push(drawn);
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::context::{MemoryFile, MessageBreakdown};

    #[test]
    fn words_wrap_between_words() {
        assert_eq!(
            wrapped("Pipe output through head, tail, or grep", 16),
            ["Pipe output", "through head,", "tail, or grep"]
        );
        assert_eq!(wrapped("", 16), Vec::<String>::new());
    }

    fn usage(percentage: u32, tools: &[(&str, u64, u64)]) -> ContextUsage {
        ContextUsage {
            total_tokens: u64::from(percentage) * 10_000,
            window: 1_000_000,
            raw_window: 1_000_000,
            percentage,
            auto_compact: true,
            auto_compact_threshold: Some(967_000),
            messages: Some(MessageBreakdown {
                tools: tools
                    .iter()
                    .map(|(name, call, result)| ToolTokens {
                        name: (*name).to_owned(),
                        call_tokens: *call,
                        result_tokens: *result,
                    })
                    .collect(),
                ..MessageBreakdown::default()
            }),
            ..ContextUsage::default()
        }
    }

    fn here() -> Places {
        Places {
            folder: Some("/home/you/todo".to_owned()),
            home: Some("/home/you".to_owned()),
        }
    }

    fn titles(found: &[Suggestion]) -> Vec<&str> {
        found.iter().map(|s| s.title.as_str()).collect()
    }

    #[test]
    fn tools_over_the_line_are_named_with_what_they_would_save() {
        let usage = usage(
            75,
            &[
                ("Bash", 0, 180_200),
                ("Read", 0, 160_400),
                ("Grep", 1_900, 74_300),
            ],
        );
        let found = suggestions(&usage, &here(), false);
        assert_eq!(
            titles(&found),
            [
                "Bash results using 180k tokens (18%)",
                "Read results using 160k tokens (16%)"
            ]
        );
        assert_eq!(found[0].severity, Severity::Warning);
        assert_eq!(found[0].savings, Some(90_100));
        assert_eq!(found[1].severity, Severity::Note);
        assert_eq!(found[1].savings, Some(48_120));
        assert!(
            found[0]
                .detail
                .starts_with("Pipe output through head, tail, or grep")
        );
        assert!(
            found[0]
                .detail
                .contains("large files \u{2014} use Read with offset/limit")
        );
        assert_eq!(found[1].tool.as_deref(), Some("Read"));
    }

    /// A tool's calls and results count together, as Claude Code adds them.
    #[test]
    fn a_tool_is_measured_by_its_calls_and_results_together() {
        let usage = usage(40, &[("Grep", 60_000, 95_000), ("WebFetch", 0, 150_000)]);
        let found = suggestions(&usage, &here(), false);
        assert_eq!(
            titles(&found),
            [
                "WebFetch results using 150k tokens (15%)",
                "Grep results using 155k tokens (16%)"
            ],
            "the larger saving first: 60k before 46.5k"
        );
        assert_eq!(found[0].savings, Some(60_000));
        assert_eq!(found[1].savings, Some(46_500));
    }

    #[test]
    fn a_tool_no_rule_knows_is_named_from_a_fifth_of_the_window() {
        let under = usage(40, &[("mcp__docs__search", 0, 190_000)]);
        assert!(suggestions(&under, &here(), false).is_empty());
        let over = usage(40, &[("mcp__docs__search", 0, 210_000)]);
        let found = suggestions(&over, &here(), false);
        assert_eq!(
            titles(&found),
            ["mcp__docs__search using 210k tokens (21%)"]
        );
        assert_eq!(
            found[0].detail,
            "This tool is consuming a significant portion of context."
        );
        assert_eq!(found[0].savings, Some(42_000));
        // Too few tokens, however large the share of a small window.
        let mut small = usage(40, &[("Bash", 0, 9_000)]);
        small.raw_window = 20_000;
        assert!(suggestions(&small, &here(), false).is_empty());
    }

    #[test]
    fn file_reads_are_named_only_where_read_itself_is_not() {
        let usage = usage(40, &[("Read", 2_000, 60_000)]);
        let found = suggestions(&usage, &here(), false);
        assert_eq!(titles(&found), ["File reads using 60k tokens (6%)"]);
        assert_eq!(found[0].savings, Some(18_000));
        assert!(found[0].detail.starts_with("If you are re-reading files"));
    }

    #[test]
    fn large_memory_files_are_named_with_the_three_largest() {
        let mut usage = usage(40, &[]);
        usage.memory_files = [
            ("/home/you/.claude/CLAUDE.md", 12_000),
            ("/home/you/todo/CLAUDE.md", 30_000),
            ("/home/you/todo/CLAUDE.local.md", 600),
            ("/home/you/todo/docs/CLAUDE.md", 9_000),
        ]
        .into_iter()
        .map(|(path, tokens)| MemoryFile {
            path: path.to_owned(),
            scope: "Project".to_owned(),
            tokens,
        })
        .collect();
        let found = suggestions(&usage, &here(), false);
        assert_eq!(titles(&found), ["Memory files using 51.6k tokens (5%)"]);
        assert_eq!(
            found[0].detail,
            "Largest: CLAUDE.md (30k), ~/.claude/CLAUDE.md (12k), docs/CLAUDE.md (9k). Use \
             /memory to review and prune stale entries."
        );
        assert_eq!(found[0].savings, Some(15_480));
    }

    #[test]
    fn a_nearly_full_context_reads_first_and_says_what_compacting_does() {
        let mut usage = usage(94, &[("Bash", 0, 241_600)]);
        let found = suggestions(&usage, &here(), false);
        assert_eq!(
            titles(&found),
            [
                "Context is 94% full",
                "Bash results using 242k tokens (24%)"
            ]
        );
        assert!(found[0].detail.starts_with("Autocompact will trigger soon"));
        assert_eq!(found[0].savings, None);

        usage.auto_compact = false;
        let found = suggestions(&usage, &here(), false);
        assert_eq!(
            found[0].detail,
            "Autocompact is disabled. Use /compact to free space, or enable autocompact in /config."
        );
        assert_eq!(
            suggestions(&usage, &here(), true)[0].detail,
            "Compaction is disabled."
        );
        // Over the window is the red line's to say.
        usage.total_tokens = 1_000_001;
        assert_eq!(
            titles(&suggestions(&usage, &here(), false)),
            ["Bash results using 242k tokens (24%)"]
        );
    }

    #[test]
    fn a_half_full_context_without_auto_compact_gets_a_note() {
        let mut usage = usage(60, &[]);
        usage.auto_compact = false;
        let found = suggestions(&usage, &here(), false);
        assert_eq!(titles(&found), ["Autocompact is disabled"]);
        assert_eq!(found[0].severity, Severity::Note);
        assert!(suggestions(&usage, &here(), true).is_empty());
        usage.percentage = 49;
        assert!(suggestions(&usage, &here(), false).is_empty());
    }
}
