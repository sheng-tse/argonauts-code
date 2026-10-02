//! The narrator's spend cap, `narrator.spend_cap`: `"0.50 usd"` or `"200k tokens"`. Narrator
//! spend is counted apart from the run's.

use argonauts_core::Spend;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpendCap {
    Usd(f64),
    /// Uncached input plus output tokens, the count token budgets use.
    Tokens(u64),
}

/// Reads `"0.50 usd"`, `"$0.50"`, `"200k tokens"`, or `"1.5m tokens"`, in any case.
pub fn parse(text: &str) -> Result<SpendCap, String> {
    let lower = text.trim().to_ascii_lowercase();
    let unreadable = || {
        format!(
            "narrator.spend_cap {text:?} isn't a cap; write it like \"0.50 usd\" or \"200k tokens\""
        )
    };
    if let Some(amount) = lower.strip_prefix('$') {
        return usd(amount.trim()).ok_or_else(unreadable);
    }
    if let Some(amount) = lower.strip_suffix("usd") {
        return usd(amount.trim()).ok_or_else(unreadable);
    }
    let tokens = lower
        .strip_suffix("tokens")
        .or_else(|| lower.strip_suffix("token"));
    if let Some(amount) = tokens {
        return count(amount.trim()).ok_or_else(unreadable);
    }
    Err(unreadable())
}

/// Where the narrator's spend stands against its cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Under,
    Reached,
    /// A dollar cap with some usage that came without a cost and had no price, so the dollars
    /// are only a lower bound and the cap can't be trusted to trip.
    CostUnknown,
}

pub fn standing(cap: SpendCap, spent: &Spend) -> Standing {
    match cap {
        SpendCap::Usd(limit) if spent.cost_usd >= limit => Standing::Reached,
        SpendCap::Usd(_) if spent.cost_partial => Standing::CostUnknown,
        SpendCap::Tokens(limit) if spent.billable_tokens() >= limit => Standing::Reached,
        SpendCap::Usd(_) | SpendCap::Tokens(_) => Standing::Under,
    }
}

/// Reached, or a dollar cap that can't count the spend.
pub fn reached(cap: SpendCap, spent: &Spend) -> bool {
    standing(cap, spent) != Standing::Under
}

/// Spend so far against the cap, such as `$0.06 of $0.50`.
pub fn describe(cap: SpendCap, spent: &Spend) -> String {
    match cap {
        SpendCap::Usd(limit) => format!("${:.2} of ${limit:.2}", spent.cost_usd),
        SpendCap::Tokens(limit) => format!("{} of {limit} tokens", spent.billable_tokens()),
    }
}

/// Adds one session's spend to the narrator's total.
pub fn add(total: &mut Spend, more: &Spend) {
    total.input_tokens = total.input_tokens.saturating_add(more.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(more.output_tokens);
    total.cached_tokens = total.cached_tokens.saturating_add(more.cached_tokens);
    total.cost_usd += more.cost_usd;
    total.cost_partial |= more.cost_partial;
    total.cost_estimated |= more.cost_estimated;
}

fn usd(amount: &str) -> Option<SpendCap> {
    let value: f64 = amount.parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(SpendCap::Usd(value))
}

fn count(amount: &str) -> Option<SpendCap> {
    let (number, scale) = if let Some(number) = amount.strip_suffix('k') {
        (number, 1_000.0)
    } else if let Some(number) = amount.strip_suffix('m') {
        (number, 1_000_000.0)
    } else {
        (amount, 1.0)
    };
    let value: f64 = number.trim().parse().ok()?;
    let tokens = (value * scale).round();
    // Checked above, so the cast neither wraps nor saturates.
    (tokens.is_finite() && tokens >= 0.0 && tokens <= u64::MAX as f64)
        .then_some(SpendCap::Tokens(tokens as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrator_cap_parser() {
        assert_eq!(parse("0.50 usd"), Ok(SpendCap::Usd(0.5)));
        assert_eq!(parse("  2 USD "), Ok(SpendCap::Usd(2.0)));
        assert_eq!(parse("0.5usd"), Ok(SpendCap::Usd(0.5)));
        assert_eq!(parse("$1.25"), Ok(SpendCap::Usd(1.25)));
        assert_eq!(parse("200k tokens"), Ok(SpendCap::Tokens(200_000)));
        assert_eq!(parse("1.5m tokens"), Ok(SpendCap::Tokens(1_500_000)));
        assert_eq!(parse("5000 tokens"), Ok(SpendCap::Tokens(5_000)));
        assert_eq!(parse("1 token"), Ok(SpendCap::Tokens(1)));
        assert_eq!(parse("0 usd"), Ok(SpendCap::Usd(0.0)));
        for bad in [
            "", "lots", "0.50", "-1 usd", "nan usd", "inf usd", "k tokens", "2 eur",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn the_cap_compares_dollars_or_billable_tokens() {
        let spent = Spend {
            input_tokens: 1_000,
            cached_tokens: 800,
            output_tokens: 100,
            cost_usd: 0.4,
            ..Spend::default()
        };
        assert!(!reached(SpendCap::Usd(0.5), &spent));
        assert!(reached(SpendCap::Usd(0.4), &spent));
        assert!(reached(SpendCap::Tokens(300), &spent));
        assert!(!reached(SpendCap::Tokens(301), &spent));
        assert_eq!(describe(SpendCap::Usd(0.5), &spent), "$0.40 of $0.50");
    }

    #[test]
    fn a_dollar_cap_stops_when_the_cost_is_unknown() {
        let unpriced = Spend {
            input_tokens: 1_000,
            output_tokens: 100,
            cost_usd: 0.001,
            cost_partial: true,
            ..Spend::default()
        };
        assert_eq!(
            standing(SpendCap::Usd(0.5), &unpriced),
            Standing::CostUnknown
        );
        assert!(reached(SpendCap::Usd(0.5), &unpriced));
        // Tokens are counted either way.
        assert_eq!(
            standing(SpendCap::Tokens(200_000), &unpriced),
            Standing::Under
        );
        let over = Spend {
            cost_usd: 0.6,
            ..unpriced
        };
        assert_eq!(standing(SpendCap::Usd(0.5), &over), Standing::Reached);
    }
}
