// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Colour for what the commands print, by hand: a few ANSI codes, only when
//! the output is a terminal and `NO_COLOR` is not set.

use std::io::IsTerminal;

/// Whether to colour, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    color: bool,
}

impl Style {
    /// Colour if the standard output is a terminal and `NO_COLOR` is not set
    /// (<https://no-color.org>).
    pub fn for_stdout() -> Self {
        let wanted = std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
        Self {
            color: wanted && std::io::stdout().is_terminal(),
        }
    }

    pub const fn plain() -> Self {
        Self { color: false }
    }

    pub const fn colored() -> Self {
        Self { color: true }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn red(self, text: &str) -> String {
        self.paint("31", text)
    }

    pub fn green(self, text: &str) -> String {
        self.paint("32", text)
    }

    pub fn yellow(self, text: &str) -> String {
        self.paint("33", text)
    }

    pub fn cyan(self, text: &str) -> String {
        self.paint("36", text)
    }

    /// A heading in a colour: `green`, `yellow`, `red`, or none for grey.
    pub fn heading(self, color: Color, text: &str) -> String {
        let code = match color {
            Color::Green => "1;32",
            Color::Yellow => "1;33",
            Color::Red => "1;31",
            Color::Grey => "1;90",
        };
        self.paint(code, text)
    }
}

/// The colours a heading can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Green,
    Yellow,
    Red,
    Grey,
}

/// How long ago a moment was, briefly: "just now", "5 min ago", "3 h ago",
/// "2 days ago".
pub fn ago(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        _ => match seconds / 86_400 {
            1 => "1 day ago".to_string(),
            days => format!("{days} days ago"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_only_when_asked() {
        assert_eq!(Style::plain().red("x"), "x");
        assert_eq!(Style::colored().red("x"), "\x1b[31mx\x1b[0m");
        assert_eq!(Style::colored().red(""), "");
    }

    #[test]
    fn ages() {
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(600), "10 min ago");
        assert_eq!(ago(7200), "2 h ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(3 * 86_400 + 5), "3 days ago");
    }
}
