//! Local coding models with tool use, best first: what `/model` recommends among the models on
//! this machine, and suggests where one isn't installed. Kept by hand with each release. Each
//! row gives the window Argonauts sets it to (at most 64k), the memory it takes there, its
//! download size as Ollama's library lists it, and whether it thinks; the sizes are estimates
//! until one has loaded here, when Ollama's own figure counts.

const GB: u64 = 1 << 30;

/// One suggested model, by its Ollama tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suggested {
    pub tag: &'static str,
    pub name: &'static str,
    pub window: u64,
    /// In bytes, at `window`.
    pub ram: u64,
    /// In bytes.
    pub download: u64,
    pub thinking: bool,
}

const fn row(
    tag: &'static str,
    name: &'static str,
    window: u64,
    ram_gb: u64,
    download_gb: u64,
    thinking: bool,
) -> Suggested {
    Suggested {
        tag,
        name,
        window,
        ram: ram_gb * GB,
        download: download_gb * GB,
        thinking,
    }
}

/// Best first.
pub const SUGGESTED: &[Suggested] = &[
    row("qwen3-coder:30b", "Qwen3 Coder 30B", 65_536, 25, 19, false),
    row("qwen3.5:27b", "Qwen3.5 27B", 65_536, 21, 17, true),
    row("devstral:24b", "Devstral 24B", 65_536, 18, 14, false),
    row("gpt-oss:20b", "GPT-OSS 20B", 65_536, 16, 14, true),
    row("qwen3:8b", "Qwen3 8B", 32_768, 7, 6, true),
    row("qwen2.5-coder:7b", "Qwen2.5 Coder 7B", 32_768, 6, 5, false),
    row("llama3.2:latest", "Llama3.2", 32_768, 4, 2, false),
];

/// Where `tag` stands on the list, best first: a tag counts as the model it's made from, so
/// `qwen3-coder:30b-64k` stands where `qwen3-coder:30b` does. `None` for one not on it.
pub fn rank(tag: &str) -> Option<usize> {
    let tag = tag.strip_suffix(":latest").unwrap_or(tag);
    SUGGESTED.iter().position(|row| {
        let base = row.tag.strip_suffix(":latest").unwrap_or(row.tag);
        tag == base
            || tag
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_stands_where_its_model_does() {
        assert_eq!(rank("qwen3-coder:30b"), Some(0));
        assert_eq!(rank("qwen3-coder:30b-64k"), Some(0));
        assert_eq!(rank("llama3.2"), rank("llama3.2:latest"));
        assert_eq!(rank("gemma3:12b"), None);
        assert_eq!(rank("qwen3:8b-q8"), Some(4));
    }

    #[test]
    fn the_list_is_best_first_with_one_row_a_model() {
        for (at, row) in SUGGESTED.iter().enumerate() {
            assert_eq!(rank(row.tag), Some(at), "{}", row.tag);
            assert!(row.window <= 65_536 && row.download < row.ram);
        }
    }
}
