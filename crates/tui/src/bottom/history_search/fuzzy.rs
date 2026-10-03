//! Fuzzy matching for the prompt search: the query's letters in order, ignoring case. A run of
//! letters together, or letters that start words, scores higher than letters strewn apart.

const MATCHED: i64 = 16;
const TOGETHER: i64 = 12;
const WORD_START: i64 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i64,
    /// Char indices into the text, ascending.
    pub positions: Vec<usize>,
}

/// The query's letters, folded once for every prompt it is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query(Vec<char>);

impl Query {
    pub fn new(query: &str) -> Self {
        Self(query.chars().map(fold).collect())
    }

    /// `None` when the text lacks one of the query's letters in order. An empty query matches
    /// everything with no letters to show.
    pub fn find(&self, text: &str) -> Option<FuzzyMatch> {
        let query = &self.0;
        if query.is_empty() {
            return Some(FuzzyMatch {
                score: 0,
                positions: Vec::new(),
            });
        }
        // Most prompts miss a letter, and telling so needs no copy of the text.
        if !in_order(query, text) {
            return None;
        }
        let original: Vec<char> = text.chars().collect();
        let folded: Vec<char> = original.iter().copied().map(fold).collect();
        let mut best =
            substring(query, &folded).map(|start| (start..start + query.len()).collect());
        if best.is_none() {
            best = tightest(query, &folded);
        }
        let positions: Vec<usize> = best?;
        let score = score(&positions, &original);
        Some(FuzzyMatch { score, positions })
    }
}

#[cfg(test)]
pub fn fuzzy_match(query: &str, text: &str) -> Option<FuzzyMatch> {
    Query::new(query).find(text)
}

fn in_order(query: &[char], text: &str) -> bool {
    let mut letters = query.iter();
    let mut want = letters.next();
    for c in text.chars() {
        let Some(&next) = want else {
            break;
        };
        if fold(c) == next {
            want = letters.next();
        }
    }
    want.is_none()
}

/// One char for one char, so indices still line up with the text.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Where the query sits whole, preferring a place that starts a word.
fn substring(query: &[char], text: &[char]) -> Option<usize> {
    let mut first = None;
    for start in 0..=text.len().checked_sub(query.len())? {
        if text[start..start + query.len()] == *query {
            if is_word_start(text, start) {
                return Some(start);
            }
            first.get_or_insert(start);
        }
    }
    first
}

/// The letters in order, then pulled together: the first end found, and from it back to the
/// latest start that still holds them all.
fn tightest(query: &[char], text: &[char]) -> Option<Vec<usize>> {
    let mut next = 0;
    let mut end = None;
    for (i, &c) in text.iter().enumerate() {
        if c == query[next] {
            next += 1;
            if next == query.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;
    let mut positions = Vec::with_capacity(query.len());
    let mut want = query.len();
    for i in (0..=end).rev() {
        if want > 0 && text[i] == query[want - 1] {
            want -= 1;
            positions.push(i);
        }
    }
    positions.reverse();
    Some(positions)
}

fn score(positions: &[usize], text: &[char]) -> i64 {
    let mut score = 0;
    for (n, &at) in positions.iter().enumerate() {
        score += MATCHED;
        if is_word_start(text, at) {
            score += WORD_START;
        }
        if n > 0 {
            let gap = at - positions[n - 1] - 1;
            if gap == 0 {
                score += TOGETHER;
            } else {
                score -= gap.min(32) as i64;
            }
        }
    }
    // A match early in the prompt edges out the same match later.
    score - positions.first().map_or(0, |&at| at.min(16) as i64)
}

fn is_word_start(text: &[char], at: usize) -> bool {
    let Some(&c) = text.get(at) else {
        return false;
    };
    match at.checked_sub(1).map(|before| text[before]) {
        None => true,
        Some(before) if !before.is_alphanumeric() => c.is_alphanumeric(),
        Some(before) => before.is_lowercase() && c.is_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positions(query: &str, text: &str) -> Option<Vec<usize>> {
        fuzzy_match(query, text).map(|found| found.positions)
    }

    fn score(query: &str, text: &str) -> i64 {
        fuzzy_match(query, text).unwrap().score
    }

    #[test]
    fn letters_match_in_order_ignoring_case() {
        assert_eq!(positions("FxT", "fix the tests"), Some(vec![0, 2, 4]));
        assert_eq!(positions("tf", "fix the tests"), None);
        assert_eq!(positions("", "anything"), Some(Vec::new()));
        assert_eq!(positions("a", ""), None);
    }

    #[test]
    fn a_whole_word_wins_over_the_first_place_it_appears() {
        // "test" is inside "latest" first, but "tests" starts a word.
        assert_eq!(positions("test", "latest tests"), Some(vec![7, 8, 9, 10]));
    }

    #[test]
    fn strewn_letters_pull_together() {
        // The first `f` would stretch the match over the whole prompt.
        assert_eq!(
            positions("fix", "for a while, fix it"),
            Some(vec![13, 14, 15])
        );
        assert_eq!(positions("fxi", "f fix i"), Some(vec![2, 4, 6]));
    }

    #[test]
    fn together_and_word_starts_score_higher() {
        assert!(score("merge", "merge the run") > score("merge", "my extra great engine"));
        assert!(score("ft", "fix tests") > score("ft", "aftermath"));
        assert!(score("run", "run it") > score("run", "please, run it"));
        // camelCase humps start words too.
        assert_eq!(positions("rs", "readSettings"), Some(vec![0, 4]));
        assert!(score("rs", "readSettings") > score("rs", "readsettings"));
    }

    #[test]
    fn positions_count_chars_not_bytes() {
        assert_eq!(positions("测试", "修复导出测试"), Some(vec![4, 5]));
        assert_eq!(positions("é", "café au lait"), Some(vec![3]));
    }
}
