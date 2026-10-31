//! Claude Code counts a top-level numbered list with numbers, a nested one with letters, and
//! one nested twice with roman numerals, so the levels read apart at a glance.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Numbering {
    Digits,
    Letters,
    Roman,
}

impl Numbering {
    /// `depth` counts the list items this list sits inside. A list that starts at zero keeps
    /// digits, since letters and roman numerals have no zero.
    pub(super) fn for_list(depth: usize, start: u64) -> Self {
        match depth {
            1 if start >= 1 => Numbering::Letters,
            2 if start >= 1 => Numbering::Roman,
            _ => Numbering::Digits,
        }
    }

    /// The marker for item `number`, such as `3. `, `c. `, or `iii. `.
    pub(super) fn marker(self, number: u64) -> String {
        let label = match self {
            Numbering::Letters if number >= 1 => letters(number),
            Numbering::Roman if (1..=3999).contains(&number) => roman(number),
            _ => number.to_string(),
        };
        format!("{label}. ")
    }
}

/// `a` to `z`, then `aa`, `ab`, and on, as spreadsheet columns count.
fn letters(mut number: u64) -> String {
    let mut out = Vec::new();
    while number > 0 {
        number -= 1;
        out.push(b'a' + (number % 26) as u8);
        number /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn roman(mut number: u64) -> String {
    const PARTS: [(u64, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, part) in PARTS {
        while number >= value {
            out.push_str(part);
            number -= value;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_levels_count_differently() {
        assert_eq!(Numbering::for_list(0, 1), Numbering::Digits);
        assert_eq!(Numbering::for_list(1, 1), Numbering::Letters);
        assert_eq!(Numbering::for_list(2, 1), Numbering::Roman);
        assert_eq!(Numbering::for_list(3, 1), Numbering::Digits);
        assert_eq!(Numbering::for_list(1, 0), Numbering::Digits);
    }

    #[test]
    fn markers_read_as_expected() {
        let letters: Vec<String> = [1, 2, 26, 27, 52, 703]
            .into_iter()
            .map(|n| Numbering::Letters.marker(n))
            .collect();
        assert_eq!(letters, ["a. ", "b. ", "z. ", "aa. ", "az. ", "aaa. "]);
        let roman: Vec<String> = [1, 4, 9, 14, 40, 1994, 4000]
            .into_iter()
            .map(|n| Numbering::Roman.marker(n))
            .collect();
        assert_eq!(
            roman,
            [
                "i. ",
                "iv. ",
                "ix. ",
                "xiv. ",
                "xl. ",
                "mcmxciv. ",
                "4000. "
            ]
        );
        assert_eq!(Numbering::Digits.marker(10), "10. ");
    }
}
