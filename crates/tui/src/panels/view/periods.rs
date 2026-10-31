//! The keys that change the period, the same on every tab of the usage panel that has one, as
//! Claude Code's `/stats` has them: `r` cycles to the next period and Shift+R to the one
//! before. Each tab brings its own periods, in the order it lists them. On the tabs that list
//! Today, Week, Month and All, 1 to 4 also pick one outright, which no key line says.

use argonauts_core::runs::api::UsagePeriod;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What every key line of a tab with periods starts with.
pub const CYCLE_HINT: &str = "r to cycle dates";

/// The periods of the Usage, Limits and Stats tabs, as their row of tabs writes them.
pub const PERIODS: [(UsagePeriod, &str); 4] = [
    (UsagePeriod::Today, "Today"),
    (UsagePeriod::Week, "Week"),
    (UsagePeriod::Month, "Month"),
    (UsagePeriod::All, "All"),
];

/// How far `key` steps along a tab's periods: `r` one on, Shift+R one back. `None` for any
/// other key, Ctrl+R among them, which reads a tab's numbers again.
pub fn step(key: KeyEvent) -> Option<isize> {
    let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
    match key.code {
        KeyCode::Char('r') if key.modifiers.is_empty() => Some(1),
        // A terminal sends Shift+R as `R`, with or without the modifier, or as `r` with it.
        KeyCode::Char('r' | 'R') if plain => Some(-1),
        _ => None,
    }
}

/// The period `by` steps along `order` from `current`, wrapping at both ends. A period that
/// isn't in `order` steps from its start.
pub fn cycled<P: Copy + PartialEq>(order: &[P], current: P, by: isize) -> P {
    let Some(len) = isize::try_from(order.len()).ok().filter(|len| *len > 0) else {
        return current;
    };
    let at = order
        .iter()
        .position(|period| *period == current)
        .unwrap_or(0) as isize;
    order[(at + by).rem_euclid(len) as usize]
}

/// A tab's keys with the period hint first, wherever the tab's own list has it.
pub fn hint_first(mut keys: Vec<String>) -> Vec<String> {
    keys.retain(|key| key != CYCLE_HINT);
    keys.insert(0, CYCLE_HINT.to_owned());
    keys
}

/// The period a key asks for on a tab that lists Today, Week, Month and All: the next or the
/// one before with `r` and Shift+R, or the one 1 to 4 name.
pub fn asked(key: KeyEvent, current: UsagePeriod) -> Option<UsagePeriod> {
    if let Some(by) = step(key) {
        let order = PERIODS.map(|(period, _)| period);
        return Some(cycled(&order, current, by));
    }
    if !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('1') => Some(UsagePeriod::Today),
        KeyCode::Char('2') => Some(UsagePeriod::Week),
        KeyCode::Char('3') => Some(UsagePeriod::Month),
        KeyCode::Char('4') => Some(UsagePeriod::All),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn r_cycles_forward_and_shift_r_back_wrapping_at_the_ends() {
        let r = key(KeyCode::Char('r'), KeyModifiers::NONE);
        let shift_r = key(KeyCode::Char('R'), KeyModifiers::SHIFT);
        assert_eq!(asked(r, UsagePeriod::Today), Some(UsagePeriod::Week));
        assert_eq!(asked(r, UsagePeriod::All), Some(UsagePeriod::Today));
        assert_eq!(asked(shift_r, UsagePeriod::Week), Some(UsagePeriod::Today));
        assert_eq!(asked(shift_r, UsagePeriod::Today), Some(UsagePeriod::All));
        // Some terminals report the capital with no modifier, and some the small letter
        // with one.
        for back in [
            key(KeyCode::Char('R'), KeyModifiers::NONE),
            key(KeyCode::Char('r'), KeyModifiers::SHIFT),
        ] {
            assert_eq!(step(back), Some(-1), "{back:?}");
        }
    }

    #[test]
    fn ctrl_r_is_not_a_period_key() {
        let ctrl_r = key(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(step(ctrl_r), None);
        assert_eq!(asked(ctrl_r, UsagePeriod::Week), None);
    }

    #[test]
    fn one_to_four_pick_a_period_outright() {
        let pick = |c| asked(key(KeyCode::Char(c), KeyModifiers::NONE), UsagePeriod::All);
        assert_eq!(pick('1'), Some(UsagePeriod::Today));
        assert_eq!(pick('2'), Some(UsagePeriod::Week));
        assert_eq!(pick('3'), Some(UsagePeriod::Month));
        assert_eq!(pick('4'), Some(UsagePeriod::All));
        assert_eq!(pick('5'), None);
    }

    #[test]
    fn the_hint_comes_first_and_once() {
        let keys = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            hint_first(keys(&["↑  tabs", CYCLE_HINT, "ctrl+s to copy"])),
            keys(&[CYCLE_HINT, "↑  tabs", "ctrl+s to copy"])
        );
        assert_eq!(
            hint_first(keys(&["↑↓  card"])),
            keys(&[CYCLE_HINT, "↑↓  card"])
        );
    }

    #[test]
    fn any_list_of_periods_cycles_in_its_own_order() {
        let order = ["all", "week", "month"];
        assert_eq!(cycled(&order, "all", 1), "week");
        assert_eq!(cycled(&order, "all", -1), "month");
        assert_eq!(cycled(&order, "month", 1), "all");
        assert_eq!(cycled(&order, "gone", 1), "week");
        assert_eq!(cycled::<&str>(&[], "all", 1), "all");
    }
}
