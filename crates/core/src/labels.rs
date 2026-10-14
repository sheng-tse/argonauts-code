//! Labels: the one-token names agents and commands go by in the conversation, the panel, and the
//! workflow view, such as `fix:slugify`, `read:anthropic`, or `review:median:r2`. They're
//! `phase:item`, lowercase, and unique in a run. Task ids stay where ids belong (Ctrl+O, the
//! agent view's id line, and commands).
//!
//! A step's `label` is a pattern. Code fills `{phase}`, `{item}`, `{provider}`, `{n}`, `{round}`,
//! `{try}`, and `{branch}`, adds `:r2` from the second round on, and `-2` when two agents
//! would share a label.

use unicode_width::UnicodeWidthStr;

use crate::names::RoleName;
use crate::workflow::StepKind;

/// The widest a label may be, in terminal cells.
pub const MAX_CELLS: usize = 20;

/// What a label pattern's placeholders stand for. Unset ones fill from the phase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelParts<'a> {
    /// The phase name, such as `Fix`.
    pub phase: &'a str,
    /// The task or plan item, such as `slugify`.
    pub item: Option<&'a str>,
    /// The model's provider, such as `anthropic`.
    pub provider: Option<&'a str>,
    /// The agent's number among the step's agents, from 1.
    pub n: Option<u32>,
    /// 1 the first time.
    pub round: u32,
    /// A goal's try.
    pub try_number: Option<u32>,
    /// The branch a merge lands on.
    pub branch: Option<&'a str>,
}

/// The pattern a step takes when the main agent left its label out.
pub fn default_pattern(kind: StepKind, role: Option<RoleName>) -> &'static str {
    match (kind, role) {
        (StepKind::Read, _) => "read:{provider}",
        (StepKind::Compare, _) => "compare:{n}",
        (StepKind::Draft, Some(RoleName::Orchestrator)) => "plan:orchestrator",
        (StepKind::Draft, _) => "draft:{provider}",
        (StepKind::Map, _) => "map:{n}",
        (StepKind::Critique, _) => "critique:{provider}",
        (StepKind::Judge, _) => "judge:{provider}",
        (StepKind::Synthesize, _) => "{phase}:{provider}",
        (StepKind::Review, _) => "review:{item}",
        (StepKind::Checks, _) => "checks:{item}",
        (StepKind::GoalCheck, _) => "check:final",
        (StepKind::Merge, _) => "merge:{branch}",
        _ => "{phase}:{item}",
    }
}

/// The label a build task's checks take, beside its worker's.
pub fn checks(item: &str) -> String {
    format!("checks:{}", slug(item, 3))
}

/// The label a build task's review takes, beside its worker's.
pub fn review(item: &str) -> String {
    format!("review:{}", slug(item, 3))
}

/// Fills a pattern. Anything a placeholder needs that isn't known falls back to the agent's
/// number, so a label is never left with braces in it.
pub fn fill(pattern: &str, parts: &LabelParts<'_>) -> String {
    let n = parts.n.unwrap_or(1).to_string();
    let try_number = parts.try_number.map(|a| a.to_string());
    let filled = pattern
        .replace("{phase}", &slug(parts.phase, 2))
        .replace(
            "{item}",
            &parts.item.map(|i| slug(i, 3)).unwrap_or(n.clone()),
        )
        .replace(
            "{provider}",
            &parts.provider.map(|f| slug(f, 2)).unwrap_or(n.clone()),
        )
        .replace("{try}", try_number.as_deref().unwrap_or(&n))
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

/// Why a label the main agent wrote can't be used, or `None` when it can.
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
        provider: Some("anthropic"),
        n: Some(1),
        round: 1,
        try_number: Some(1),
        branch: Some("main"),
    };
    let unknown = pattern
        .split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .find(|name| !["phase", "item", "provider", "n", "round", "try", "branch"].contains(name));
    if let Some(name) = unknown {
        return Some(format!(
            "label `{pattern}` uses an unknown placeholder `{{{name}}}`"
        ));
    }
    let raw = pattern
        .replace("{phase}", "phase")
        .replace("{item}", "item")
        .replace("{provider}", "anthropic")
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

/// A slug an id can carry, as `task-<slug>`: lowercase ASCII letters and digits joined by `-`,
/// from the first `words` words that have any, short enough to leave room in an id for the
/// `task-` before it and a `-2` after it. A request written in another script has none, and
/// its slug is `task`.
pub fn id_slug(text: &str, words: usize) -> String {
    /// `task-` and a `-99` leave this much of an id's 64 bytes.
    const MOST: usize = crate::ids::MAX_ID_LEN - "task-".len() - "-99".len();
    let slug = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(words)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-");
    let slug = slug[..slug.len().min(MOST)].trim_end_matches('-');
    if slug.is_empty() {
        "task".to_owned()
    } else {
        slug.to_owned()
    }
}

/// A task's item from its title: its first two words, less a leading verb that only repeats
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

/// `label` then `suffix` in [`MAX_CELLS`]: whole words go from the item's end, so a label never
/// stops mid-word and the round or the repeat number always shows. An item that is one long
/// word is cut, never mid-character.
fn fit(label: &str, suffix: &str) -> String {
    let room = MAX_CELLS.saturating_sub(suffix.width());
    if label.width() <= room {
        return format!("{label}{suffix}");
    }
    let item_starts = label.find(':').map_or(0, |colon| colon + 1);
    let mut kept = label;
    while kept.width() > room {
        // A label code gives outright, such as `commit message`, has its words apart.
        match kept.rfind(['-', ' ']) {
            Some(at) if at > item_starts => kept = &kept[..at],
            _ => break,
        }
    }
    let mut out = String::new();
    for c in kept.chars() {
        let next = format!("{out}{c}");
        if next.width() > room {
            break;
        }
        out = next;
    }
    format!("{}{suffix}", out.trim_end_matches([':', '-', ' ']))
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
    fn an_id_slug_is_ascii_whatever_the_request_is_written_in() {
        assert_eq!(id_slug("修復登入頁面的錯誤", 4), "task");
        assert_eq!(id_slug("Réparer l'élément du formulaire", 4), "r-parer-l-l");
        assert_eq!(id_slug("Fix the login page", 4), "fix-the-login-page");
        let long = id_slug(&"abcdefghij ".repeat(10), 10);
        assert!(long.len() <= MAX_SLUG, "{long}");
        for slug in [id_slug("修復 x", 4), long] {
            crate::TaskId::new(format!("task-{slug}-99")).expect("a valid id");
        }
    }

    /// What `task-` and a `-99` leave of an id.
    const MAX_SLUG: usize = crate::ids::MAX_ID_LEN - 8;

    #[test]
    fn patterns_fill_from_the_item_and_the_provider() {
        let fix = LabelParts {
            item: Some("slugify"),
            ..parts("Fix")
        };
        assert_eq!(fill("fix:{item}", &fix), "fix:slugify");
        assert_eq!(fill("{phase}:{item}", &fix), "fix:slugify");
        let read = LabelParts {
            provider: Some("Anthropic"),
            ..parts("Understand")
        };
        assert_eq!(fill("read:{provider}", &read), "read:anthropic");
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
        let taken = ["read:anthropic", "read:anthropic-2"];
        assert_eq!(
            unique("read:anthropic", |l| taken.contains(&l)),
            "read:anthropic-3"
        );
        assert_eq!(unique("read:openai", |l| taken.contains(&l)), "read:openai");
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
    fn a_label_too_wide_drops_whole_words() {
        let build = LabelParts {
            item: Some("add-explicit-burst"),
            ..parts("Build")
        };
        assert_eq!(fill("build:{item}", &build), "build:add-explicit");
        let review = LabelParts {
            item: Some("document-the-burst"),
            round: 2,
            ..parts("Review")
        };
        assert_eq!(fill("review:{item}", &review), "review:document:r2");
        assert_eq!(
            unique("build:add-explicit", |label| label == "build:add-explicit"),
            "build:add-explicit-2"
        );
        // One word wider than the label is all there is to cut.
        let word = LabelParts {
            item: Some("internationalization"),
            ..parts("Review")
        };
        assert_eq!(fill("review:{item}", &word), "review:international");
        assert_eq!(
            unique("fix after main moved", |label| label
                == "fix after main moved"),
            "fix after main-2"
        );
        // The phase's own dash is never where a label ends.
        let phase = LabelParts {
            item: Some("verbose-flag-parsing"),
            ..parts("Sign off")
        };
        assert_eq!(fill("{phase}:{item}", &phase), "sign-off:verbose");
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
    fn items_come_from_task_titles() {
        assert_eq!(
            item_from_title("Fix slugify's dashes", "Fix"),
            "slugify-dashes"
        );
        assert_eq!(item_from_title("Median of empty lists", "Fix"), "median-of");
        assert_eq!(item_from_title("Fix", "Fix"), "fix");
    }
}
