//! The places the built-in cards show: above the input and the Overview's tabs. Each is an
//! ordered list, and the lists are the only record of where a card shows.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::super::Span;
use super::widgets::WidgetName;

/// The band between the working line and the input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AboveInputSettings {
    /// The mods above the input, top to bottom
    pub mods: Vec<WidgetName>,
    /// Show the band. Ctrl+X Ctrl+A flips it for the session
    pub shown: bool,
    /// From this width, two half-width mods share a row; 0 never
    pub side_by_side_at: u16,
    /// Narrower than this, every mod draws its one line
    pub min_width: u16,
    /// The most rows the band takes, always inside ui.live_area_max
    pub max_rows: u16,
    /// What the band does while a question has the keys: fold, keep, or hide
    pub when_question: WhenQuestion,
    /// What the hidden band's one line shows: the numbers or a count
    pub hidden_line: HiddenLine,
    /// Values redraw at most this often, such as "1s"
    pub refresh: Span,
    /// A change of height waits this long, then lands in one frame, such as "150ms"
    pub settle: Span,
}

impl Default for AboveInputSettings {
    fn default() -> Self {
        Self {
            mods: Vec::new(),
            shown: true,
            side_by_side_at: 120,
            min_width: 60,
            max_rows: 14,
            when_question: WhenQuestion::Fold,
            hidden_line: HiddenLine::Numbers,
            refresh: Span::new("1s"),
            settle: Span::new("150ms"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WhenQuestion {
    #[default]
    Fold,
    Keep,
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HiddenLine {
    #[default]
    Numbers,
    Count,
}

/// The Overview's tabs, /status, /usage, /context, and /limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ViewsSettings {
    /// From this width, two half-width cards share a row
    pub side_by_side_at: u16,
    /// The Status tab
    pub status: ViewTab,
    /// The Usage tab
    pub usage: UsageTab,
    /// The Context tab
    pub context: ViewTab,
    /// The Limits tab
    #[serde(rename = "limits")]
    pub budget: ViewTab,
}

impl Default for ViewsSettings {
    fn default() -> Self {
        use WidgetName::*;
        Self {
            side_by_side_at: 100,
            status: ViewTab::with(&[Run, Budget, Quota, Waiting, Recent]),
            usage: UsageTab {
                mods: vec![Quota, Spend, Tokens, History, Runs],
                open: ViewOpen::View,
                period: UsagePeriod::Day,
            },
            context: ViewTab::with(&[Context, Sessions]),
            budget: ViewTab::with(&[Budget, Tasks, Limits]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ViewTab {
    /// The tab's mods, top to bottom
    pub mods: Vec<WidgetName>,
    /// How its command opens it: as a panel (view or dialog), or printed into the conversation
    pub open: ViewOpen,
}

impl ViewTab {
    fn with(widgets: &[WidgetName]) -> Self {
        Self {
            mods: widgets.to_vec(),
            open: ViewOpen::View,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UsageTab {
    /// The tab's mods, top to bottom
    pub mods: Vec<WidgetName>,
    /// How /usage opens it: as a panel (view or dialog), or printed into the conversation
    pub open: ViewOpen,
    /// The period it opens on: day, week, month, or all
    pub period: UsagePeriod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewOpen {
    /// A panel under the working line that stays live, as every tab opens since a command
    /// works while an answer runs.
    #[default]
    View,
    /// The same panel. The word is kept for settings written when the two differed.
    Dialog,
    /// Into the conversation, once, as the command's `print` does.
    Print,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsagePeriod {
    #[default]
    Day,
    Week,
    Month,
    All,
}

impl UsagePeriod {
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "day" => Some(UsagePeriod::Day),
            "week" => Some(UsagePeriod::Week),
            "month" => Some(UsagePeriod::Month),
            "all" => Some(UsagePeriod::All),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PanelBar {
    #[default]
    Agents,
    #[serde(rename = "tasks")]
    Cards,
    #[serde(rename = "spend")]
    Budget,
    Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PanelContext {
    #[default]
    Pills,
    Words,
    Off,
}
