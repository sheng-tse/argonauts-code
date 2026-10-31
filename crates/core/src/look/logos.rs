//! Which logo goes before a CLI's or a model's name: always whoever serves the model. The screen
//! draws it in cells or as a picture; where no logo file exists it draws the serving agent's
//! colour dot.

use serde::{Deserialize, Serialize};

use crate::base::names::AgentId;
use crate::look::display_names::maker_of;

/// The logo for one CLI or model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Logo {
    /// The CLI or endpoint that serves it, whose colour the dot takes where no picture is drawn.
    pub agent: AgentId,
    /// `None` where no logo file exists, as for a maker the logos don't cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<LogoMark>,
}

impl Logo {
    /// The serving agent's colour dot, with no picture.
    pub fn dot(agent: &str) -> Self {
        Self {
            agent: AgentId(agent.to_owned()),
            mark: None,
        }
    }
}

/// A logo the app ships a picture of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogoMark {
    ClaudeCode,
    Codex,
    Antigravity,
    OpenCode,
    Ollama,
    Qwen,
    Kimi,
    OpenAi,
    Mistral,
    Meta,
    OpenRouter,
}

impl LogoMark {
    pub const ALL: [Self; 11] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::Antigravity,
        Self::OpenCode,
        Self::Ollama,
        Self::Qwen,
        Self::Kimi,
        Self::OpenAi,
        Self::Mistral,
        Self::Meta,
        Self::OpenRouter,
    ];

    /// The stem its picture files are named by, as in `claudecode`.
    pub fn stem(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claudecode",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::OpenCode => "opencode",
            Self::Ollama => "ollama",
            Self::Qwen => "qwen",
            Self::Kimi => "kimi",
            Self::OpenAi => "openai",
            Self::Mistral => "mistral",
            Self::Meta => "meta",
            Self::OpenRouter => "openrouter",
        }
    }

    /// Seats of one model are numbered up to this in their logos' pictures, as far as the
    /// circled numbers go.
    pub const MOST_NUMBERED: u8 = 9;

    /// The id the screen sends this logo's picture under, plain or numbered `number` (1 to
    /// [`Self::MOST_NUMBERED`]), in its ink for light screens where `light_ink`. A test that
    /// reads the pictures back finds each logo by it.
    pub fn picture_id(self, number: Option<u8>, light_ink: bool) -> u32 {
        let ink = u32::from(light_ink);
        match number {
            None => PLAIN_PICTURES + self as u32 * 2 + ink,
            Some(number) => {
                let place = self as u32 * u32::from(Self::MOST_NUMBERED)
                    + u32::from(number.saturating_sub(1));
                NUMBERED_PICTURES + place * 2 + ink
            }
        }
    }

    /// The logo picture `id` shows in either ink, and its number for a numbered one. `None`
    /// for any other picture.
    pub fn of_picture(id: u32) -> Option<(Self, Option<u8>)> {
        let place = (id & !1).checked_sub(PLAIN_PICTURES)? / 2;
        let marks = Self::ALL.len() as u32;
        let most = u32::from(Self::MOST_NUMBERED);
        if place < marks {
            return Some((*Self::ALL.get(usize::try_from(place).ok()?)?, None));
        }
        let place = (id & !1).checked_sub(NUMBERED_PICTURES)? / 2;
        let mark = *Self::ALL.get(usize::try_from(place / most).ok()?)?;
        let number = u8::try_from(place % most + 1).ok()?;
        Some((mark, Some(number)))
    }

    /// The logo of a CLI, endpoint or local runtime by its id, where one ships.
    pub fn of_agent(agent: &str) -> Option<Self> {
        Some(match agent {
            "claude-code" | "claude" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "antigravity-cli" => Self::Antigravity,
            "opencode" => Self::OpenCode,
            "openrouter" => Self::OpenRouter,
            "ollama" => Self::Ollama,
            _ => return None,
        })
    }
}

/// The logo before `model` as `cli` runs it, which is whoever serves the model. Claude Code,
/// Codex, agy and an endpoint serve what they run, so the logo is theirs. OpenCode runs each
/// model through a provider, so its rows show who stands behind it: OpenCode's own for a model
/// OpenCode Zen serves, a host's own logo (Ollama's llama for a model on this computer,
/// OpenRouter's), else the maker's where one ships, else the colour dot.
pub fn logo_for(cli: &str, model: Option<&str>) -> Logo {
    let cli = cli.trim();
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    let mark = match model {
        Some(model) if cli == OPENCODE => behind_opencode(model),
        _ => LogoMark::of_agent(cli),
    };
    Logo {
        agent: AgentId(cli.to_owned()),
        mark,
    }
}

const OPENCODE: &str = "opencode";

/// The logos' pictures go out under ids from here, two to a logo: its ink for dark screens, then
/// a one-colour logo's ink for light ones. They stay within the 24 bits a placeholder's colour
/// carries, apart from the mascot's and the map's.
const PLAIN_PICTURES: u32 = 0x4c_4f00;

/// The numbered logos' pictures, two for each logo and number, clear of the plain logos' ids.
const NUMBERED_PICTURES: u32 = 0x4c_5000;

/// The cell, in pixels, the logos' two-cell pictures are drawn for. A terminal with larger
/// cells takes the pictures drawn at twice the size.
pub const LOGO_CELL_PIXELS: (u16, u16) = (18, 42);

/// The logo of whoever serves a model OpenCode runs, by the provider its id starts with, as
/// `ollama/qwen3-coder:30b`.
fn behind_opencode(model: &str) -> Option<LogoMark> {
    match model.split_once('/') {
        Some((OPENCODE, _)) => Some(LogoMark::OpenCode),
        Some((provider, _)) if serves_other_makers(provider) => LogoMark::of_agent(provider),
        _ => maker_of(model).and_then(|maker| maker.logo),
    }
}

/// A provider that serves other makers' models, whose logo says where a model runs: a server
/// on this computer, or OpenRouter.
fn serves_other_makers(provider: &str) -> bool {
    crate::agents::runtimes::runtime(provider).is_some() || provider == "openrouter"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every logo's pictures, plain and numbered, in both inks, have ids of their own that read
    /// back as that logo and number.
    #[test]
    fn each_logo_picture_reads_back_from_its_id() {
        let mut ids = std::collections::BTreeSet::new();
        for mark in LogoMark::ALL {
            for number in std::iter::once(None).chain((1..=LogoMark::MOST_NUMBERED).map(Some)) {
                for light in [false, true] {
                    let id = mark.picture_id(number, light);
                    assert!(ids.insert(id), "{mark:?} {number:?} {light}");
                    assert!(id < 1 << 24);
                    assert_eq!(LogoMark::of_picture(id), Some((mark, number)));
                }
            }
        }
        assert_eq!(LogoMark::ClaudeCode.picture_id(None, false), 0x4c_4f00);
        assert_eq!(LogoMark::ClaudeCode.picture_id(Some(2), false), 0x4c_5002);
        assert_eq!(LogoMark::of_picture(0x4c_4f00 - 1), None);
    }

    #[test]
    fn a_model_shows_the_logo_of_the_cli_that_serves_it() {
        let logo = logo_for("codex", Some("gpt-5.6-luna"));
        assert_eq!(logo.agent, AgentId("codex".into()));
        assert_eq!(logo.mark, Some(LogoMark::Codex));
        assert_eq!(
            logo_for("antigravity-cli", None).mark,
            Some(LogoMark::Antigravity)
        );
        // agy serves the Claude models it lists, so they take its logo.
        assert_eq!(
            logo_for("antigravity-cli", Some("claude-sonnet-4-6")).mark,
            Some(LogoMark::Antigravity)
        );
        assert_eq!(
            logo_for("claude-code", Some("claude-mythos-5-1")).mark,
            Some(LogoMark::ClaudeCode)
        );
        assert_eq!(
            logo_for("ollama", Some("qwen3-coder:30b")).mark,
            Some(LogoMark::Ollama)
        );
    }

    /// A model OpenCode runs shows who stands behind it, and every logo keeps OpenCode's
    /// colour for its dot.
    #[test]
    fn a_model_opencode_runs_shows_who_serves_it() {
        for (model, mark) in [
            (None, Some(LogoMark::OpenCode)),
            (Some("ollama/qwen3-coder:30b"), Some(LogoMark::Ollama)),
            (Some("ollama/llama3.2"), Some(LogoMark::Ollama)),
            (Some("lmstudio/qwen3-coder-30b"), None),
            (
                Some("openrouter/qwen/qwen3-coder"),
                Some(LogoMark::OpenRouter),
            ),
            (Some("openrouter/z-ai/glm-5"), Some(LogoMark::OpenRouter)),
            (Some("opencode/kimi-k2"), Some(LogoMark::OpenCode)),
            (Some("moonshotai/kimi-k2"), Some(LogoMark::Kimi)),
            (Some("openai/gpt-5.6"), Some(LogoMark::OpenAi)),
            (Some("mistral/devstral-2"), Some(LogoMark::Mistral)),
            (
                Some("opencode/glm-5.3-flash-free"),
                Some(LogoMark::OpenCode),
            ),
            (Some("opencode/big-pickle"), Some(LogoMark::OpenCode)),
            (Some("anthropic/claude-sonnet-4-6"), None),
            (Some("  "), Some(LogoMark::OpenCode)),
        ] {
            let logo = logo_for("opencode", model);
            assert_eq!(logo.mark, mark, "{model:?}");
            assert_eq!(logo.agent, AgentId("opencode".into()), "{model:?}");
        }
    }

    #[test]
    fn an_agent_with_no_logo_file_gets_its_colour_dot() {
        assert_eq!(logo_for("lmstudio", Some("glm-5")), Logo::dot("lmstudio"));
        assert_eq!(logo_for("my-endpoint", None).mark, None);
    }

    #[test]
    fn every_logo_has_its_own_file_stem() {
        let mut stems: Vec<&str> = LogoMark::ALL.iter().map(|mark| mark.stem()).collect();
        stems.sort_unstable();
        stems.dedup();
        assert_eq!(stems.len(), LogoMark::ALL.len());
    }

    #[test]
    fn a_dot_is_written_without_a_mark() {
        let written = serde_json::to_value(Logo::dot("vllm")).unwrap();
        assert_eq!(written, serde_json::json!({ "agent": "vllm" }));
        let codex = serde_json::to_value(logo_for("codex", None)).unwrap();
        assert_eq!(codex["mark"], "codex");
    }
}
