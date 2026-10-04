//! Labels: the one-token names agents and commands go by in the chat, the panel, and the
//! workflow view, such as `fix:slugify`, `read:claude`, or `review:median:r2`. They're
//! `phase:item`, lowercase, and unique in a run. Card ids stay where ids belong (Ctrl+O, the
//! agent view's id line, and commands).
//!
//! A step's `label` is a pattern. Code fills `{phase}`, `{item}`, `{family}`, `{n}`, `{round}`,
//! `{attempt}`, and `{branch}`, adds `:r2` from the second round on, and `-2` when two agents
//! would share a label.

use unicode_width::UnicodeWidthStr;

use crate::names::RoleName;
use crate::workflow::StepKind;

/// The widest a label may be, in terminal cells.
pub const MAX_CELLS: usize = 20;

/// What a label pattern's placeholders stand for. Unset ones fill from the phase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelParts<'a> {
    /// The phase title, such as `Fix`.
    pub phase: &'a str,
    /// The card or plan item, such as `slugify`.
    pub item: Option<&'a str>,
    /// The model family, such as `claude`.
    pub family: Option<&'a str>,
    /// The agent's number among the step's agents, from 1.
    pub n: Option<u32>,
    /// 1 the first time.
    pub round: u32,
    /// A goal's attempt.
    pub attempt: Option<u32>,
    /// The branch a merge lands on.
    pub branch: Option<&'a str>,
}

/// The pattern a step takes when the lead left its label out.
pub fn default_pattern(kind: StepKind, role: Option<RoleName>) -> &'static str {
    match (kind, role) {
        (StepKind::Restate, _) => "read:{family}",
        (StepKind::Differ, _) => "compare:{n}",
        (StepKind::Draft, Some(RoleName::Coordinator)) => "plan:coordinator",
        (StepKind::Draft, _) => "draft:{family}",
        (StepKind::Map, _) => "map:{n}",
        (StepKind::Refute, _) => "critique:{family}",
        (StepKind::Judge, _) => "judge:{family}",
        (StepKind::Synthesize, _) => "{phase}:{family}",
        (StepKind::Review, _) => "review:{item}",
        (StepKind::Gate, _) => "tests:{item}",
        (StepKind::Check, _) => "check:final",
        (StepKind::Merge, _) => "merge:{branch}",
        _ => "{phase}:{item}",
    }
}

/// The label a build card's gates take, beside its owner's.
pub fn gates(item: &str) -> String {
    format!("tests:{}", slug(item, 3))
}

/// The label a build card's review takes, beside its owner's.
pub fn review(item: &str) -> String {
    format!("review:{}", slug(item, 3))
}

/// Fills a pattern. Anything a placeholder needs that isn't known falls back to the agent's
/// number, so a label is never left with braces in it.
pub fn fill(pattern: &str, parts: &LabelParts<'_>) -> String {
    let n = parts.n.unwrap_or(1).to_string();
    let attempt = parts.attempt.map(|a| a.to_string());
    let filled = pattern
        .replace("{phase}", &slug(parts.phase, 2))
        .replace(
            "{item}",
            &parts.item.map(|i| slug(i, 3)).unwrap_or(n.clone()),
        )
        .replace(
            "{family}",
            &parts.family.map(|f| slug(f, 2)).unwrap_or(n.clone()),
        )
        .replace("{attempt}", attempt.as_deref().unwrap_or(&n))
        .replace(
            "{branch}",
            &parts.branch.map(|b| slug(b, 3)).unwrap_or("main".into()),
        )
        .replace("{round}", &parts.round.max(1).to_string())
        .replace("{n}", &n);
    with_round(&tidy(&filled), parts.round)
}

/// `review:median:r2` from the second round on, within [`MAX_CELLS`]. A pattern that already
/// says `{round}` keeps its own.
pub fn with_round(label: &str, round: u32) -> String {
    let suffix = format!(":r{round}");
    if round <= 1 || label.ends_with(&suffix) {
        fit(label, "")
    } else {
        fit(label, &suffix)
    }
}

/// `label`, or `label-2`, `label-3`, … when the run already uses it, within [`MAX_CELLS`].
pub fn unique(label: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(label) {
        return label.to_owned();
    }
    (2..)
        .map(|n| fit(label, &format!("-{n}")))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| label.to_owned())
}

/// Why a label the lead wrote can't be used, or `None` when it can.
pub fn problem(label: &str) -> Option<String> {
    if label.is_empty() {
        return Some("a label is empty".to_owned());
    }
    if label.width() > MAX_CELLS {
        return Some(format!("label `{label}` is wider than {MAX_CELLS} cells"));
    }
    if label.chars().any(|c| c.is_whitespace()) {
        return Some(format!("label `{label}` has a space; labels are one token"));
    }
    if label.chars().any(char::is_uppercase) {
        return Some(format!("label `{label}` isn't lowercase"));
    }
    if label.starts_with(':') || label.ends_with(':') {
        return Some(format!("label `{label}` starts or ends with `:`"));
    }
    if !label.contains(':') {
        return Some(format!(
            "label `{label}` has no `:`; write `phase:item`, such as `fix:slugify`"
        ));
    }
    None
}

/// Why a step's label pattern can't be used, judged on what it fills to with every
/// placeholder set to something short.
pub fn pattern_problem(pattern: &str) -> Option<String> {
    let sample = LabelParts {
        phase: "phase",
        item: Some("item"),
        family: Some("claude"),
        n: Some(1),
        round: 1,
        attempt: Some(1),
        branch: Some("main"),
    };
    let unknown = pattern
        .split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .find(|name| {
            !["phase", "item", "family", "n", "round", "attempt", "branch"].contains(name)
        });
    if let Some(name) = unknown {
        return Some(format!(
            "label `{pattern}` uses an unknown placeholder `{{{name}}}`"
        ));
    }
    let raw = pattern
        .replace("{phase}", "phase")
        .replace("{item}", "item")
        .replace("{family}", "claude")
        .replace("{branch}", "main")
        .replace(['{', '}'], "");
    problem(&raw).or_else(|| problem(&fill(pattern, &sample)))
}

/// A name in label form: lowercase words of letters and digits joined by `-`, at most `words`
/// of them. Other scripts' letters stay as they are.
pub fn slug(text: &str, words: usize) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(words)
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// A card's item from its title: its first two words, less a leading verb that only repeats
/// the phase (`Fix slugify's dashes` under `Fix` is `slugify-dashes`).
pub fn item_from_title(title: &str, phase: &str) -> String {
    let phase = phase.to_lowercase();
    let title = title.replace("'s ", " ").replace("’s ", " ");
    let words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let start = usize::from(words.len() > 1 && words[0] == phase);
    words[start..]
        .iter()
        .take(2)
        .cloned()
        .collect::<Vec<_>>()
        .join("-")
}

/// Collapses doubled separators and trims stray ones, so a placeholder that filled empty never
/// leaves `fix::` or `-`.
fn tidy(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars() {
        let separator = matches!(c, ':' | '-');
        if separator && (out.is_empty() || out.ends_with([':', '-'])) {
            continue;
        }
        out.push(c);
    }
    out.trim_end_matches([':', '-']).to_owned()
}

/// `label` then `suffix` in [`MAX_CELLS`]: the label is cut from the item's end, never
/// mid-character, so the round or the repeat number always shows.
fn fit(label: &str, suffix: &str) -> String {
    let room = MAX_CELLS.saturating_sub(suffix.width());
    if label.width() <= room {
        return format!("{label}{suffix}");
    }
    let mut out = String::new();
    for c in label.chars() {
        let next = format!("{out}{c}");
        if next.width() > room {
            break;
        }
        out = next;
    }
    format!("{}{suffix}", out.trim_end_matches([':', '-']))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(phase: &'static str) -> LabelParts<'static> {
        LabelParts {
            phase,
            round: 1,
            ..LabelParts::default()
        }
    }

    #[test]
    fn patterns_fill_from_the_item_and_the_family() {
        let fix = LabelParts {
            item: Some("slugify"),
            ..parts("Fix")
        };
        assert_eq!(fill("fix:{item}", &fix), "fix:slugify");
        assert_eq!(fill("{phase}:{item}", &fix), "fix:slugify");
        let read = LabelParts {
            family: Some("Claude"),
            ..parts("Understand")
        };
        assert_eq!(fill("read:{family}", &read), "read:claude");
        assert_eq!(
            fill(
                "compare:{n}",
                &LabelParts {
                    n: Some(2),
                    ..parts("Understand")
                }
            ),
            "compare:2"
        );
        assert_eq!(fill("merge:{branch}", &parts("Merge")), "merge:main");
    }

    #[test]
    fn later_rounds_and_repeats_stay_unique() {
        let review = LabelParts {
            item: Some("median"),
            round: 2,
            ..parts("Review")
        };
        assert_eq!(fill("review:{item}", &review), "review:median:r2");
        let taken = ["read:claude", "read:claude-2"];
        assert_eq!(
            unique("read:claude", |l| taken.contains(&l)),
            "read:claude-3"
        );
        assert_eq!(unique("read:gpt", |l| taken.contains(&l)), "read:gpt");
        // The round and the repeat number show, inside twenty cells.
        let long = LabelParts {
            item: Some("verbose-flag-parsing"),
            round: 2,
            ..parts("Review")
        };
        let label = fill("review:{item}", &long);
        assert!(
            label.ends_with(":r2") && label.width() <= MAX_CELLS,
            "{label}"
        );
        let taken = ["build:verbose-flag-p"];
        let label = unique("build:verbose-flag-p", |l| taken.contains(&l));
        assert!(
            label.ends_with("-2") && label.width() <= MAX_CELLS,
            "{label}"
        );
    }

    #[test]
    fn labels_fit_twenty_cells_and_never_keep_braces() {
        let long = LabelParts {
            item: Some("the median of an empty list"),
            ..parts("Implementation")
        };
        let label = fill("{phase}:{item}", &long);
        assert!(label.width() <= MAX_CELLS, "{label}");
        assert!(!label.contains('{'));
        assert_eq!(fill("fix:{item}", &parts("Fix")), "fix:1");
        let cjk = LabelParts {
            item: Some("中文標籤名稱很長很長的名字"),
            ..parts("修正")
        };
        assert!(fill("{phase}:{item}", &cjk).width() <= MAX_CELLS);
    }

    #[test]
    fn written_labels_are_checked() {
        assert_eq!(problem("fix:slugify"), None);
        assert!(problem("Fix:slugify").is_some());
        assert!(problem("fix slugify").is_some());
        assert!(problem("a-label-that-is-much-too-long").is_some());
        assert_eq!(pattern_problem("fix:{item}"), None);
        assert!(pattern_problem("fix:{thing}").is_some());
        assert!(problem("fix").is_some());
        assert!(pattern_problem("fix").is_some());
        assert!(pattern_problem("{item}").is_some());
    }

    #[test]
    fn items_come_from_card_titles() {
        assert_eq!(
            item_from_title("Fix slugify's dashes", "Fix"),
            "slugify-dashes"
        );
        assert_eq!(item_from_title("Median of empty lists", "Fix"), "median-of");
        assert_eq!(item_from_title("Fix", "Fix"), "fix");
    }
}
