//! Server-sent events, split into lines across chunk boundaries.

use std::fmt;

/// A line or an event's data longer than this ends the stream, so an endpoint that never
/// sends a newline or a blank line can't take all memory.
pub(crate) const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// One complete line of the stream, kept raw for the log. `event` holds the data of the event
/// this line completes: the joined `data:` lines, dispatched at the blank line that ends them.
/// `raw` is `None` only for an event the stream never closed with a blank line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseLine {
    pub raw: Option<String>,
    pub event: Option<String>,
}

/// What grew past [`MAX_EVENT_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overflow {
    Line,
    Event,
}

impl fmt::Display for Overflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            Overflow::Line => "a line",
            Overflow::Event => "an event",
        };
        let mib = MAX_EVENT_BYTES / (1024 * 1024);
        write!(f, "the endpoint streamed {what} longer than {mib} MiB")
    }
}

#[derive(Debug)]
pub(crate) struct SseDecoder {
    pending: Vec<u8>,
    /// `pending[..scanned]` holds no newline, so each push scans only the bytes it adds.
    scanned: usize,
    data: Option<String>,
    limit: usize,
    overflow: Option<Overflow>,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::with_limit(MAX_EVENT_BYTES)
    }
}

impl SseDecoder {
    fn with_limit(limit: usize) -> Self {
        Self {
            pending: Vec::new(),
            scanned: 0,
            data: None,
            limit,
            overflow: None,
        }
    }

    /// The lines that `chunk` completes. Once a line or an event grows past the limit, the
    /// decoder drops what it holds and takes nothing more, and [`Self::overflow`] says so.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<SseLine> {
        let mut lines = Vec::new();
        if self.overflow.is_some() {
            return lines;
        }
        self.pending.extend_from_slice(chunk);
        let mut start = 0;
        while let Some(found) = self.pending[self.scanned..]
            .iter()
            .position(|&b| b == b'\n')
        {
            let end = self.scanned + found;
            if end - start > self.limit {
                self.fail(Overflow::Line);
                return lines;
            }
            let line = &self.pending[start..end];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = String::from_utf8_lossy(line).into_owned();
            start = end + 1;
            self.scanned = start;
            lines.push(self.line(line));
            if self.overflow.is_some() {
                return lines;
            }
        }
        // One shift per chunk, not per line.
        self.pending.drain(..start);
        self.scanned = self.pending.len();
        if self.pending.len() > self.limit {
            self.fail(Overflow::Line);
        }
        lines
    }

    /// Whatever is left when the stream ends: an unterminated last line, and an event that
    /// never got its closing blank line.
    pub(crate) fn finish(&mut self) -> Vec<SseLine> {
        let mut lines = Vec::new();
        if self.overflow.is_some() {
            return lines;
        }
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.scanned = 0;
            let text = String::from_utf8_lossy(&rest);
            lines.push(self.line(text.trim_end_matches('\r').to_owned()));
            if self.overflow.is_some() {
                return lines;
            }
        }
        if let Some(data) = self.data.take() {
            lines.push(SseLine {
                raw: None,
                event: Some(data),
            });
        }
        lines
    }

    /// Why decoding stopped early, if it did.
    pub(crate) fn overflow(&self) -> Option<Overflow> {
        self.overflow
    }

    fn fail(&mut self, overflow: Overflow) {
        self.overflow = Some(overflow);
        self.pending = Vec::new();
        self.scanned = 0;
        self.data = None;
    }

    fn line(&mut self, raw: String) -> SseLine {
        if raw.is_empty() {
            return SseLine {
                raw: Some(raw),
                event: self.data.take(),
            };
        }
        if let Some(value) = field_value(&raw, "data") {
            match &mut self.data {
                Some(data) if data.len() + 1 + value.len() > self.limit => {
                    self.fail(Overflow::Event);
                }
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            }
        }
        SseLine {
            raw: Some(raw),
            event: None,
        }
    }
}

/// The value of a `name: value` line, with the one optional space after the colon removed.
fn field_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(name)?;
    if rest.is_empty() {
        return Some("");
    }
    let value = rest.strip_prefix(':')?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(lines: &[SseLine]) -> Vec<&str> {
        lines.iter().filter_map(|l| l.event.as_deref()).collect()
    }

    #[test]
    fn lines_split_across_chunks_come_out_whole() {
        let mut decoder = SseDecoder::default();
        let mut lines = decoder.push(b"data: {\"a\"");
        assert!(lines.is_empty());
        lines.extend(decoder.push(b":1}\r\n\r\nevent: x\ndata: [DONE]\n"));
        lines.extend(decoder.push(b"\n"));
        let raws: Vec<&str> = lines.iter().filter_map(|l| l.raw.as_deref()).collect();
        assert_eq!(
            raws,
            ["data: {\"a\":1}", "", "event: x", "data: [DONE]", ""]
        );
        assert_eq!(events(&lines), ["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn data_lines_of_one_event_are_joined() {
        let mut decoder = SseDecoder::default();
        let lines = decoder.push(b"data: {\"a\":\ndata:1}\n: keep-alive\n\n");
        assert_eq!(events(&lines), ["{\"a\":\n1}"]);
        assert_eq!(lines[2].raw.as_deref(), Some(": keep-alive"));
    }

    #[test]
    fn an_unterminated_tail_is_kept_at_the_end() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: tail").is_empty());
        let lines = decoder.finish();
        assert_eq!(lines[0].raw.as_deref(), Some("data: tail"));
        assert_eq!(lines[1].raw, None);
        assert_eq!(events(&lines), ["tail"]);
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn multibyte_text_split_mid_character_survives() {
        let text = "data: héllo\n\n".as_bytes();
        let mut decoder = SseDecoder::default();
        let mut lines = decoder.push(&text[..8]);
        lines.extend(decoder.push(&text[8..]));
        assert_eq!(events(&lines), ["héllo"]);
    }

    #[test]
    fn a_long_line_in_small_chunks_comes_out_whole() {
        let mut stream = b"data: ".to_vec();
        stream.extend(std::iter::repeat_n(b'x', 64 * 1024));
        stream.extend_from_slice(b"\r\n\n");
        let mut decoder = SseDecoder::default();
        let mut lines = Vec::new();
        for chunk in stream.chunks(7) {
            lines.extend(decoder.push(chunk));
        }
        assert_eq!(events(&lines), ["x".repeat(64 * 1024)]);
        assert_eq!(decoder.overflow(), None);
    }

    #[test]
    fn a_line_past_the_limit_ends_decoding() {
        let mut decoder = SseDecoder::with_limit(16);
        assert_eq!(events(&decoder.push(b"data: a\n\n")), ["a"]);
        assert!(decoder.push(b"data: 0123456789").is_empty());
        assert_eq!(decoder.overflow(), None);
        assert!(decoder.push(b"abc").is_empty());
        assert_eq!(decoder.overflow(), Some(Overflow::Line));
        assert!(decoder.push(b"\n\ndata: b\n\n").is_empty());
        assert!(decoder.finish().is_empty());

        // A whole line in one chunk is held to the limit too.
        let mut decoder = SseDecoder::with_limit(16);
        let lines = decoder.push(b"data: a\n\n: 0123456789abcdef\ndata: b\n\n");
        assert_eq!(events(&lines), ["a"]);
        assert_eq!(decoder.overflow(), Some(Overflow::Line));
    }

    #[test]
    fn an_event_past_the_limit_ends_decoding() {
        let mut decoder = SseDecoder::with_limit(16);
        let lines = decoder.push(b"data: 01234567\ndata: 01234567\n\n");
        assert_eq!(events(&lines), Vec::<&str>::new());
        assert_eq!(decoder.overflow(), Some(Overflow::Event));
        assert!(decoder.finish().is_empty());

        // The unterminated tail can overflow the event as well.
        let mut decoder = SseDecoder::with_limit(16);
        assert!(events(&decoder.push(b"data: 01234567\ndata: 01234567")).is_empty());
        assert!(events(&decoder.finish()).is_empty());
        assert_eq!(decoder.overflow(), Some(Overflow::Event));
    }
}
