//! Chat between the viewer and the host: plain text, directly between the
//! two computers. On the viewer, the session and its chat window talk
//! through the window's standard input and output, one [`Line`] per line.

/// The longest message, in characters.
pub const MAX_CHARS: usize = 1000;

/// A message as it may be sent: control characters other than line breaks
/// removed, trimmed, at most [`MAX_CHARS`]. `None` when nothing is left.
pub fn clean(text: &str) -> Option<String> {
    let text: String = text
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .take(MAX_CHARS)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// What passes between a viewer's session and its chat window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// Written at this computer (the window echoes it).
    Mine(String),
    /// Written at the other computer.
    Theirs(String),
    /// The session ended, and why.
    Ended(String),
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, c == '\\') {
            (_, true) => match chars.next() {
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            (c, false) => out.push(c),
        }
    }
    out
}

impl Line {
    /// One line of text, without its line break.
    pub fn encode(&self) -> String {
        match self {
            Line::Mine(text) => format!("mine {}", escape(text)),
            Line::Theirs(text) => format!("theirs {}", escape(text)),
            Line::Ended(why) => format!("ended {}", escape(why)),
        }
    }

    pub fn decode(line: &str) -> Option<Line> {
        let (kind, text) = line.split_once(' ').unwrap_or((line, ""));
        let text = unescape(text);
        Some(match kind {
            "mine" => Line::Mine(text),
            "theirs" => Line::Theirs(text),
            "ended" => Line::Ended(text),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_cleaned() {
        assert_eq!(clean("  hi \u{7}there\r\n ").as_deref(), Some("hi there"));
        assert_eq!(clean("two\nlines").as_deref(), Some("two\nlines"));
        assert_eq!(clean(" \t\u{1b}"), None);
        assert_eq!(clean(&"x".repeat(5000)).map(|t| t.len()), Some(MAX_CHARS));
    }

    #[test]
    fn lines_survive_the_pipe() {
        for line in [
            Line::Mine("hello".into()),
            Line::Theirs("two\nlines with a \\ and \\n".into()),
            Line::Ended("the host ended the session".into()),
            Line::Theirs(String::new()),
        ] {
            let encoded = line.encode();
            assert!(!encoded.contains('\n'), "{encoded}");
            assert_eq!(Line::decode(&encoded), Some(line));
        }
        assert_eq!(Line::decode("shout hi"), None);
    }
}
