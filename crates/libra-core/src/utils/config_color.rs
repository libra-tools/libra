//! Git-compatible color value parsing (port of `color.c:color_parse_mem`).
//!
//! `libra config --type=color` needs the exact ANSI sequence Git prints for a
//! configured color. The grammar is small but fiddly: whitespace-separated
//! words, an optional leading `reset`, one foreground and one background color,
//! and boolean attributes with an optional `no`/`no-` prefix. This module is a
//! faithful port so the output matches `git config --type=color` byte for byte.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorKind {
    Unspecified,
    Normal,
    Ansi(u8),
    Color256(u8),
    Rgb(u8, u8, u8),
}

impl ColorKind {
    /// Git's `color_empty`: an unspecified or plain `normal` color emits nothing.
    fn is_empty(self) -> bool {
        matches!(self, Self::Unspecified | Self::Normal)
    }
}

/// A value that is not a Git color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorParseError {
    value: String,
}

impl fmt::Display for ColorParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid color value: {}", self.value)
    }
}

impl std::error::Error for ColorParseError {}

fn hexval(c: u8) -> u32 {
    match c {
        b'0'..=b'9' => (c - b'0') as u32,
        b'a'..=b'f' => (c - b'a' + 10) as u32,
        b'A'..=b'F' => (c - b'A' + 10) as u32,
        // Any non-hex digit must make the whole value fail (`val & ~0xff`),
        // exactly like Git's unsigned `hexval` sentinel.
        _ => 0x100,
    }
}

fn get_hex_color(bytes: &[u8], cursor: &mut usize, width: usize) -> Option<u8> {
    let first = *bytes.get(*cursor)?;
    let last = *bytes.get(*cursor + width - 1)?;
    let val = (hexval(first) << 4) | hexval(last);
    if val & !0xff != 0 {
        return None;
    }
    *cursor += width;
    Some(val as u8)
}

/// Git's `parse_ansi_color`: `default`, an optional `bright` prefix, then the
/// eight base color names. Color names are case-insensitive.
fn parse_ansi_color(word: &str) -> Option<ColorKind> {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];

    if word.eq_ignore_ascii_case("default") {
        // 39 for foreground; the +10 background offset is applied at output.
        return Some(ColorKind::Ansi(9 + 30));
    }

    let mut offset: u16 = 30;
    let mut name = word;
    if name.len() >= 6 && name[..6].eq_ignore_ascii_case("bright") {
        offset = 90;
        name = &name[6..];
    }
    for (index, candidate) in NAMES.iter().enumerate() {
        if name.eq_ignore_ascii_case(candidate) {
            return Some(ColorKind::Ansi((index as u16 + offset) as u8));
        }
    }
    None
}

/// Git's `strtol`-based numeric branch, restricted to base 10 and a whole-word
/// match. Negative values other than `-1` are invalid.
fn parse_numeric_color(word: &str) -> Option<Option<ColorKind>> {
    let bytes = word.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (negative, digits) = match bytes[0] {
        b'-' => (true, &word[1..]),
        b'+' => (false, &word[1..]),
        _ => (false, word),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let magnitude: i64 = digits.parse().ok()?;
    let value = if negative { -magnitude } else { magnitude };

    let kind = if value < -1 {
        return None;
    } else if value < 0 {
        ColorKind::Normal
    } else if value < 8 {
        ColorKind::Ansi((value + 30) as u8)
    } else if value < 16 {
        ColorKind::Ansi((value - 8 + 90) as u8)
    } else if value < 256 {
        ColorKind::Color256(value as u8)
    } else {
        return None;
    };
    Some(Some(kind))
}

/// Git's `parse_color`: `normal`, `#rgb`/`#rrggbb`, an ANSI color, or 0-255.
fn parse_color(word: &str) -> Option<ColorKind> {
    if word.eq_ignore_ascii_case("normal") {
        return Some(ColorKind::Normal);
    }

    let bytes = word.as_bytes();
    if (bytes.len() == 7 || bytes.len() == 4) && bytes[0] == b'#' {
        let width = if bytes.len() == 7 { 2 } else { 1 };
        let mut cursor = 1;
        if let (Some(red), Some(green), Some(blue)) = (
            get_hex_color(bytes, &mut cursor, width),
            get_hex_color(bytes, &mut cursor, width),
            get_hex_color(bytes, &mut cursor, width),
        ) {
            return Some(ColorKind::Rgb(red, green, blue));
        }
    }

    if let Some(kind) = parse_ansi_color(word) {
        return Some(kind);
    }

    parse_numeric_color(word).flatten()
}

/// Git's `parse_attr`: attribute names are **case-sensitive**, with an optional
/// `no`/`no-` prefix selecting the disabling ANSI code.
fn parse_attr(word: &str) -> Option<i32> {
    const ATTRS: [(&str, i32, i32); 7] = [
        ("bold", 1, 22),
        ("dim", 2, 22),
        ("italic", 3, 23),
        ("ul", 4, 24),
        ("blink", 5, 25),
        ("reverse", 7, 27),
        ("strike", 9, 29),
    ];

    let mut name = word;
    let mut negate = false;
    if name.len() >= 2 && &name[..2] == "no" {
        name = &name[2..];
        if let Some(rest) = name.strip_prefix('-') {
            name = rest;
        }
        negate = true;
    }

    for (candidate, value, negative) in ATTRS {
        if name == candidate {
            return Some(if negate { negative } else { value });
        }
    }
    None
}

fn color_output(kind: ColorKind, background: bool) -> String {
    let offset: u16 = if background { 10 } else { 0 };
    match kind {
        ColorKind::Unspecified | ColorKind::Normal => String::new(),
        ColorKind::Ansi(value) => format!("{}", value as u16 + offset),
        ColorKind::Color256(value) => format!("{};5;{}", 38 + offset, value),
        ColorKind::Rgb(red, green, blue) => {
            format!("{};2;{};{};{}", 38 + offset, red, green, blue)
        }
    }
}

fn parse_color_value_inner(value: &str, quiet: bool) -> Result<String, ColorParseError> {
    let trimmed = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    let mut has_reset = false;
    let mut attrs: Vec<i32> = Vec::new();
    let mut fg = ColorKind::Unspecified;
    let mut bg = ColorKind::Unspecified;

    for word in trimmed.split_ascii_whitespace() {
        if word.eq_ignore_ascii_case("reset") {
            has_reset = true;
            continue;
        }
        if let Some(kind) = parse_color(word) {
            if fg == ColorKind::Unspecified {
                fg = kind;
                continue;
            }
            if bg == ColorKind::Unspecified {
                bg = kind;
                continue;
            }
            return Err(ColorParseError {
                value: value.to_string(),
            });
        }
        if let Some(attr) = parse_attr(word) {
            if !attrs.contains(&attr) {
                attrs.push(attr);
            }
            continue;
        }
        return Err(ColorParseError {
            value: value.to_string(),
        });
    }

    let _ = quiet;
    if !has_reset && attrs.is_empty() && fg.is_empty() && bg.is_empty() {
        return Ok(String::new());
    }

    attrs.sort_unstable();
    let mut out = String::from("\x1b[");
    let mut needs_separator = has_reset;
    for attr in attrs {
        if needs_separator {
            out.push(';');
        }
        needs_separator = true;
        out.push_str(&attr.to_string());
    }
    if !fg.is_empty() {
        if needs_separator {
            out.push(';');
        }
        needs_separator = true;
        out.push_str(&color_output(fg, false));
    }
    if !bg.is_empty() {
        if needs_separator {
            out.push(';');
        }
        out.push_str(&color_output(bg, true));
    }
    out.push('m');
    Ok(out)
}

/// Parse a Git color value into its ANSI escape sequence, mirroring
/// `git config --type=color`. An empty or whitespace-only value renders as the
/// empty string; an unparseable value is an error.
pub fn parse_color_value(value: &str) -> Result<String, ColorParseError> {
    parse_color_value_inner(value, false)
}

/// `git config --type=color --list` keeps only entries whose value parses.
pub fn is_valid_color(value: &str) -> bool {
    parse_color_value_inner(value, true).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_names_match_git() {
        assert_eq!(parse_color_value("red").unwrap(), "\x1b[31m");
        assert_eq!(parse_color_value("brightred").unwrap(), "\x1b[91m");
        assert_eq!(parse_color_value("default").unwrap(), "\x1b[39m");
        assert_eq!(parse_color_value("default default").unwrap(), "\x1b[39;49m");
        assert_eq!(parse_color_value("red default").unwrap(), "\x1b[31;49m");
        assert_eq!(parse_color_value("normal").unwrap(), "");
        assert_eq!(parse_color_value("reset").unwrap(), "\x1b[m");
    }

    #[test]
    fn numeric_and_rgb_match_git() {
        assert_eq!(parse_color_value("0").unwrap(), "\x1b[30m");
        assert_eq!(parse_color_value("7").unwrap(), "\x1b[37m");
        assert_eq!(parse_color_value("8").unwrap(), "\x1b[90m");
        assert_eq!(parse_color_value("15").unwrap(), "\x1b[97m");
        assert_eq!(parse_color_value("16").unwrap(), "\x1b[38;5;16m");
        assert_eq!(parse_color_value("255").unwrap(), "\x1b[38;5;255m");
        assert_eq!(parse_color_value("#ff0000").unwrap(), "\x1b[38;2;255;0;0m");
        assert_eq!(parse_color_value("#f00").unwrap(), "\x1b[38;2;255;0;0m");
        assert_eq!(parse_color_value("-1").unwrap(), "");
    }

    #[test]
    fn attributes_match_git() {
        assert_eq!(parse_color_value("bold").unwrap(), "\x1b[1m");
        assert_eq!(parse_color_value("dim").unwrap(), "\x1b[2m");
        assert_eq!(parse_color_value("italic").unwrap(), "\x1b[3m");
        assert_eq!(parse_color_value("ul").unwrap(), "\x1b[4m");
        assert_eq!(parse_color_value("blink").unwrap(), "\x1b[5m");
        assert_eq!(parse_color_value("reverse").unwrap(), "\x1b[7m");
        assert_eq!(parse_color_value("strike").unwrap(), "\x1b[9m");
        assert_eq!(parse_color_value("nobold").unwrap(), "\x1b[22m");
        assert_eq!(parse_color_value("no-bold").unwrap(), "\x1b[22m");
        assert_eq!(parse_color_value("no-ul").unwrap(), "\x1b[24m");
        assert_eq!(parse_color_value("bold red blue").unwrap(), "\x1b[1;31;44m");
    }

    #[test]
    fn invalid_values_are_rejected() {
        for value in ["bogus", "bright red", "256", "300", "#gg0000", "BOLD"] {
            assert!(parse_color_value(value).is_err(), "{value} must be invalid");
            assert!(!is_valid_color(value), "{value} must not list as a color");
        }
    }
}
