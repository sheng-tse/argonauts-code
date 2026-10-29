//! Who sits in the council as your screen draws them: each seat's model by name, the logo of
//! whoever serves it, and its number when its model sits more than once ("Opus 5.5 · 2").

use argonauts_core::display_names::{DisplayNames, after_logo};
use argonauts_core::events::council::Seating;
use argonauts_core::logos::Logo;
use argonauts_core::names::SeatId;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::kit::logo::{ModelLabel, NoPicture, logo_spans, numbered_spans};
use crate::text::str_width;
use crate::theme::{Role, Theme};

/// One seat as your screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Face {
    /// The model's whole name, as a sentence writes it: "GPT 5.6 Luna".
    pub name: String,
    /// The CLI that runs the seat, whose colour its name takes.
    pub cli: String,
    pub logo: Logo,
    /// Its place among the seats of the same model, from 1.
    pub number: Option<u8>,
}

impl Face {
    pub fn new(name: impl Into<String>, cli: impl Into<String>, logo: Logo) -> Self {
        Self {
            name: name.into(),
            cli: cli.into(),
            logo,
            number: None,
        }
    }

    /// A seat as it sat down: its model and the logo of whoever serves it, numbered when its
    /// model sits twice.
    pub fn of_seat(seat: &argonauts_core::events::council::Seat, names: &DisplayNames) -> Self {
        let model = &seat.model;
        let logo = argonauts_core::logos::logo_for(&model.agent, model.model.as_deref());
        let whole = names
            .model(&model.agent, model.model.as_deref())
            .unwrap_or_else(|| names.cli(&model.agent));
        let face = Self::new(whole, model.agent.as_str(), logo);
        match seat.number {
            Some(number) => face.numbered(number),
            None => face,
        }
    }

    pub fn numbered(mut self, number: u8) -> Self {
        self.number = Some(number);
        self
    }

    /// "Opus 5.5 · 2" for a model that sits twice, else the model's whole name, as a sentence
    /// names the seat.
    pub fn label(&self) -> String {
        self.numbered_label(self.name.clone())
    }

    /// The seat's name right after its badge: without its maker's word after its maker's own
    /// logo where that logo draws as a picture, "5.6 Luna", and whole where it doesn't.
    pub fn beside(&self, theme: &Theme) -> String {
        self.numbered_label(ModelLabel::named(&self.logo, &self.name, theme, NoPicture::Dot).name)
    }

    /// The seat's name as you may type it after `@`: its whole name, or as its badge has it.
    pub fn typed_names(&self) -> [String; 2] {
        [
            self.label(),
            self.numbered_label(after_logo(&self.logo, &self.name)),
        ]
    }

    fn numbered_label(&self, name: String) -> String {
        match self.number {
            Some(number) => format!("{name} · {number}"),
            None => name,
        }
    }

    pub fn beside_width(&self, theme: &Theme) -> usize {
        str_width(&self.beside(theme))
    }

    /// The seat's name in its CLI's colour, as a mention inside a message draws it.
    pub fn name_style(&self, theme: &Theme) -> Style {
        theme.style(Role::for_cli(&self.cli))
    }

    /// Two cells before the seat's name: its logo, or for a model that sits more than once its
    /// logo numbered, since one logo can't tell two seats of the same model apart.
    pub fn badge(&self, theme: &Theme) -> Vec<Span<'static>> {
        match self.number {
            Some(number) => numbered_spans(&self.logo, number, self.name_style(theme), theme),
            None => logo_spans(&self.logo, theme),
        }
    }

    /// Badge, blank and name, as a seat named in a message reads.
    pub fn mention(&self, theme: &Theme) -> Vec<Span<'static>> {
        let mut spans = self.badge(theme);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            self.beside(theme),
            self.name_style(theme).add_modifier(Modifier::BOLD),
        ));
        spans
    }

    /// Cells a mention takes: the badge, a blank and the name.
    pub fn mention_width(&self, theme: &Theme) -> usize {
        BADGE + 1 + self.beside_width(theme)
    }
}

/// Cells a badge takes, logo or number.
pub const BADGE: usize = 2;

/// Every seat's face, in seat order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Faces {
    seats: Vec<(SeatId, Face)>,
}

impl Faces {
    pub fn new(seats: impl IntoIterator<Item = (SeatId, Face)>) -> Self {
        Self {
            seats: seats.into_iter().collect(),
        }
    }

    /// The faces of a council as it sat down, each model named by `name` and served by `logo`.
    pub fn of(
        seating: &Seating,
        name: impl Fn(&argonauts_core::names::ModelRef) -> (String, String, Logo),
    ) -> Self {
        Self::new(seating.seats.iter().map(|seat| {
            let (model, cli, logo) = name(&seat.model);
            let mut face = Face::new(model, cli, logo);
            face.number = seat.number;
            (seat.id.clone(), face)
        }))
    }

    pub fn get(&self, seat: &SeatId) -> Option<&Face> {
        self.seats
            .iter()
            .find(|(id, _)| id == seat)
            .map(|(_, face)| face)
    }

    /// The seat's face, or a plain one naming its id for a seat the council never sat.
    pub fn face(&self, seat: &SeatId) -> Face {
        self.get(seat)
            .cloned()
            .unwrap_or_else(|| Face::new(seat.as_str(), "", Logo::dot("")))
    }

    pub fn len(&self) -> usize {
        self.seats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seats.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = &SeatId> {
        self.seats.iter().map(|(id, _)| id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SeatId, &Face)> {
        self.seats.iter().map(|(id, face)| (id, face))
    }

    /// The seats `text` names as the `@` list writes them, "@5.6 Luna", each once and in the
    /// order named.
    pub fn mentioned(&self, text: &str) -> Vec<SeatId> {
        let mut named: Vec<SeatId> = Vec::new();
        for (at, _) in text.match_indices('@') {
            // An `@` inside a word belongs to an address, not a mention.
            let starts_a_word = text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            if starts_a_word
                && let Some(seat) = self.named_at(&text[at + 1..])
                && !named.contains(seat)
            {
                named.push(seat.clone());
            }
        }
        named
    }

    /// Whether `text` starts by naming a seat, so it goes to the council with that seat named
    /// rather than to a worker by that name.
    pub fn starts_with_seat(&self, text: &str) -> bool {
        text.trim_start()
            .strip_prefix('@')
            .and_then(|rest| self.named_at(rest))
            .is_some()
    }

    /// The seat whose label `rest` starts with, up to the end of a word. The longest label
    /// wins, so a name that starts another seat's name never takes its mention. Case doesn't
    /// matter, since a name typed by hand may not match the list's.
    fn named_at(&self, rest: &str) -> Option<&SeatId> {
        self.seats
            .iter()
            .flat_map(|(id, face)| face.typed_names().map(|label| (id, label)))
            .filter_map(|(id, label)| {
                let typed = rest.get(..label.len())?;
                let after = &rest[label.len()..];
                let whole = after.chars().next().is_none_or(|c| !c.is_alphanumeric());
                (whole && typed.eq_ignore_ascii_case(&label)).then_some((label.len(), id))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, id)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_that_sits_twice_reads_with_its_number() {
        let face = Face::new("Opus 5.5", "claude-code", Logo::dot("claude-code")).numbered(2);
        assert_eq!(face.label(), "Opus 5.5 · 2");
        assert_eq!(crate::kit::logo::circled(2), "②");
        let theme = Theme::dark();
        let badge: String = face
            .badge(&theme)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(badge, "② ");
        assert_eq!(
            face.mention_width(&theme),
            2 + 1 + str_width("Opus 5.5 · 2")
        );
    }

    fn council() -> Faces {
        let face = |name: &str, cli: &str| Face::new(name, cli, Logo::dot(cli));
        Faces::new([
            (
                SeatId::numbered(1),
                face("Opus 5.5", "claude-code").numbered(1),
            ),
            (SeatId::numbered(2), face("5.6 Luna", "codex")),
            (
                SeatId::numbered(3),
                face("Opus 5.5", "claude-code").numbered(2),
            ),
        ])
    }

    #[test]
    fn a_message_names_each_seat_it_mentions_once() {
        let faces = council();
        assert_eq!(
            faces.mentioned("@Opus 5.5 · 2 and @5.6 luna, why a bucket? @5.6 Luna?"),
            [SeatId::numbered(3), SeatId::numbered(2)]
        );
        // Neither a longer word nor an address names a seat.
        assert!(
            faces
                .mentioned("@5.6 Lunar or you@5.6 Luna.example")
                .is_empty()
        );
        assert!(
            faces
                .mentioned("@Opus 5.5 alone names neither Opus")
                .is_empty()
        );
    }

    #[test]
    fn only_a_message_that_starts_with_a_seat_starts_with_one() {
        let faces = council();
        assert!(faces.starts_with_seat("  @5.6 Luna why a bucket?"));
        assert!(!faces.starts_with_seat("@codex add a test"));
        assert!(!faces.starts_with_seat("Ask @5.6 Luna"));
    }
}
