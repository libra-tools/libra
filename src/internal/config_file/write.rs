//! Git-compatible config file writer (`git config --file <path> <write>`).
//!
//! The reader in [`crate::command::config`] answers reads; this module applies
//! writes with Git's placement and quoting rules:
//!
//! - an existing variable is updated in place;
//! - a new variable is appended at the end of its section's last occurrence;
//! - a missing section is created at end of file (`[section]` or
//!   `[section "subsection"]`);
//! - values are quoted only when they need it (`#`, `;`, `"`, `\`, newline, or
//!   leading/trailing whitespace).
//!
//! The command layer performs the atomic `<file>.lock` + rename write.

use std::fmt;

/// One write operation against a config file.
#[derive(Debug, Clone)]
pub enum Mutation {
    Set {
        key: String,
        value: String,
        value_pattern: Option<String>,
        replace_all: bool,
        add: bool,
    },
    Unset {
        key: String,
        value_pattern: Option<String>,
        all: bool,
    },
    RemoveSection {
        name: String,
    },
    RenameSection {
        old: String,
        new: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFileWriteError {
    message: String,
    /// Git's exit code for this failure (5 for "no such key"/"multiple values").
    exit_code: i32,
}

impl ConfigFileWriteError {
    fn new(message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            message: message.into(),
            exit_code,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

impl fmt::Display for ConfigFileWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigFileWriteError {}

/// A parsed variable line.
#[derive(Debug, Clone)]
struct ParsedLine {
    index: usize,
    section: String,
    key: String,
    value: String,
}

fn parse_section_header(inner: &str) -> Option<String> {
    let inner = inner.trim();
    if let Some((head, rest)) = inner.split_once(char::is_whitespace) {
        let sub = rest.trim().trim_matches('"');
        if !sub.is_empty() {
            return Some(format!("{}.{}", head.trim().to_ascii_lowercase(), sub));
        }
        return Some(head.trim().to_ascii_lowercase());
    }
    match inner.split_once('.') {
        Some((head, tail)) => Some(format!("{}.{}", head.to_ascii_lowercase(), tail)),
        None => Some(inner.to_ascii_lowercase()),
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if let Some(stripped) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut escaped = false;
        for ch in stripped.chars() {
            if escaped {
                out.push(match ch {
                    'n' => '\n',
                    't' => '\t',
                    'b' => '\u{8}',
                    other => other,
                });
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return out;
            } else {
                out.push(ch);
            }
        }
        return out;
    }
    match value.find(['#', ';']) {
        Some(index) => value[..index].trim_end().to_string(),
        None => value.to_string(),
    }
}

fn parse_lines(lines: &[String]) -> Vec<ParsedLine> {
    let mut parsed = Vec::new();
    let mut section = String::new();
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(inner) = rest.split(']').next()
                && let Some(parsed_section) = parse_section_header(inner)
            {
                section = parsed_section;
            }
            continue;
        }
        if section.is_empty() {
            continue;
        }
        let (name, value) = match line.split_once('=') {
            Some((name, value)) => (name.trim(), unquote(value)),
            None => (line, "true".to_string()),
        };
        parsed.push(ParsedLine {
            index,
            section: section.clone(),
            key: format!("{}.{}", section, name.to_ascii_lowercase()),
            value,
        });
    }
    parsed
}

fn section_of_key(key: &str) -> (String, String) {
    match key.rsplit_once('.') {
        Some((section, variable)) => (section.to_string(), variable.to_string()),
        None => (key.to_string(), key.to_string()),
    }
}

/// Quote a value the way Git does when it cannot be written bare.
pub fn quote_value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value != value.trim()
        || value
            .chars()
            .any(|c| matches!(c, '#' | ';' | '"' | '\\' | '\n' | '\t' | '\u{8}') || c.is_control());
    if !needs_quotes {
        return value.to_string();
    }
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn section_header(section: &str) -> String {
    match section.split_once('.') {
        Some((head, sub)) => format!("[{head} \"{sub}\"]"),
        None => format!("[{section}]"),
    }
}

fn indent_for(key: &str) -> String {
    let (_, variable) = section_of_key(key);
    format!("\t{variable} = ")
}

fn matches_value(line: &ParsedLine, value_pattern: Option<&str>) -> bool {
    match value_pattern {
        None => true,
        Some(pattern) => match regex::Regex::new(pattern) {
            Ok(re) => re.is_match(&line.value),
            Err(_) => false,
        },
    }
}

/// Apply `mutation` to the config file text, returning the new text.
pub fn apply(text: &str, mutation: &Mutation) -> Result<String, ConfigFileWriteError> {
    let mut lines: Vec<String> = if text.is_empty() {
        Vec::new()
    } else {
        text.lines().map(ToString::to_string).collect()
    };

    match mutation {
        Mutation::Set {
            key,
            value,
            value_pattern,
            replace_all,
            add,
        } => {
            let parsed = parse_lines(&lines);
            let key_lower = key.to_ascii_lowercase();
            let matches: Vec<&ParsedLine> = parsed.iter().filter(|p| p.key == key_lower).collect();
            let matching: Vec<&ParsedLine> = matches
                .into_iter()
                .filter(|p| matches_value(p, value_pattern.as_deref()))
                .collect();

            if *add {
                let (section, _) = section_of_key(key);
                insert_variable(&mut lines, &parsed, &section, key, value);
                return Ok(lines.join("\n") + "\n");
            }

            if matching.is_empty() {
                let (section, _) = section_of_key(key);
                insert_variable(&mut lines, &parsed, &section, key, value);
                return Ok(lines.join("\n") + "\n");
            }

            if matching.len() > 1 && !*replace_all && value_pattern.is_none() {
                return Err(ConfigFileWriteError::new(
                    format!(
                        "warning: {key} has multiple values\nerror: cannot overwrite multiple values with a single value"
                    ),
                    5,
                ));
            }

            // Replace the last match in place (Git replaces the first for a bare
            // set and the matching one for a value-pattern set); remove the
            // others when `--replace-all`.
            let replace_index = matching.last().map(|p| p.index).unwrap_or(0);
            lines[replace_index] = format!("{}{}", indent_for(key), quote_value(value));
            if *replace_all {
                let mut to_remove: Vec<usize> = matching
                    .iter()
                    .map(|p| p.index)
                    .filter(|index| *index != replace_index)
                    .collect();
                to_remove.sort_unstable_by(|a, b| b.cmp(a));
                for index in to_remove {
                    lines.remove(index);
                }
            }
            Ok(lines.join("\n") + "\n")
        }
        Mutation::Unset {
            key,
            value_pattern,
            all,
        } => {
            let parsed = parse_lines(&lines);
            let key_lower = key.to_ascii_lowercase();
            let matching: Vec<&ParsedLine> = parsed
                .iter()
                .filter(|p| p.key == key_lower && matches_value(p, value_pattern.as_deref()))
                .collect();
            if matching.is_empty() {
                return Err(ConfigFileWriteError::new(
                    format!("key '{key}' not found"),
                    5,
                ));
            }
            let mut indexes: Vec<usize> = matching
                .iter()
                .map(|p| p.index)
                .take(if *all { usize::MAX } else { 1 })
                .collect();
            indexes.sort_unstable_by(|a, b| b.cmp(a));
            for index in indexes {
                lines.remove(index);
            }
            Ok(lines.join("\n") + "\n")
        }
        Mutation::RemoveSection { name } => {
            let parsed = parse_lines(&lines);
            let section_lower = name.to_ascii_lowercase();
            let mut indexes: Vec<usize> = parsed
                .iter()
                .filter(|p| p.section == section_lower)
                .map(|p| p.index)
                .collect();
            // Remove the section headers too.
            for (index, raw) in lines.iter().enumerate() {
                let line = raw.trim();
                if let Some(rest) = line.strip_prefix('[')
                    && let Some(inner) = rest.split(']').next()
                    && parse_section_header(inner).as_deref() == Some(section_lower.as_str())
                {
                    indexes.push(index);
                }
            }
            if indexes.is_empty() {
                return Err(ConfigFileWriteError::new(
                    format!("no such section: {name}"),
                    5,
                ));
            }
            indexes.sort_unstable_by(|a, b| b.cmp(a));
            for index in indexes {
                lines.remove(index);
            }
            Ok(lines.join("\n") + "\n")
        }
        Mutation::RenameSection { old, new } => {
            let old_lower = old.to_ascii_lowercase();
            let new_lower = new.to_ascii_lowercase();
            let mut renamed = false;
            for raw in lines.iter_mut() {
                let trimmed = raw.trim();
                if let Some(rest) = trimmed.strip_prefix('[')
                    && let Some(inner) = rest.split(']').next()
                    && parse_section_header(inner).as_deref() == Some(old_lower.as_str())
                {
                    *raw = section_header(&new_lower);
                    renamed = true;
                }
            }
            if !renamed {
                return Err(ConfigFileWriteError::new(
                    format!("no such section: {old}"),
                    5,
                ));
            }
            Ok(lines.join("\n") + "\n")
        }
    }
}

fn insert_variable(
    lines: &mut Vec<String>,
    parsed: &[ParsedLine],
    section: &str,
    key: &str,
    value: &str,
) {
    let assignment = format!("{}{}", indent_for(key), quote_value(value));
    // Append after the last line of the section (variable or header).
    let mut last_index: Option<usize> = None;
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix('[')
            && let Some(inner) = rest.split(']').next()
            && parse_section_header(inner).as_deref() == Some(section)
        {
            last_index = Some(index);
        }
    }
    for entry in parsed.iter().filter(|p| p.section == section) {
        last_index = Some(last_index.map_or(entry.index, |i| i.max(entry.index)));
    }
    match last_index {
        Some(index) => lines.insert(index + 1, assignment),
        None => {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(section_header(section));
            lines.push(assignment);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(key: &str, value: &str) -> Mutation {
        Mutation::Set {
            key: key.to_string(),
            value: value.to_string(),
            value_pattern: None,
            replace_all: false,
            add: false,
        }
    }

    #[test]
    fn updates_an_existing_variable_in_place() {
        let text = "[core]\n\tbare = false\n\teditor = vi\n";
        let out = apply(text, &set("core.editor", "vim")).unwrap();
        assert_eq!(out, "[core]\n\tbare = false\n\teditor = vim\n");
    }

    #[test]
    fn appends_to_the_section_then_creates_it() {
        let text = "[a]\n\tx = 1\n[b]\n\ty = 2\n";
        let out = apply(text, &set("a.z", "9")).unwrap();
        assert_eq!(out, "[a]\n\tx = 1\n\tz = 9\n[b]\n\ty = 2\n");

        let out = apply(out.as_str(), &set("c.k", "v")).unwrap();
        assert_eq!(out, "[a]\n\tx = 1\n\tz = 9\n[b]\n\ty = 2\n\n[c]\n\tk = v\n");
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(quote_value("plain"), "plain");
        assert_eq!(quote_value("a b"), "a b");
        assert_eq!(quote_value(" lead"), "\" lead\"");
        assert_eq!(quote_value("a;b"), "\"a;b\"");
        assert_eq!(quote_value("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_value(""), "\"\"");
    }

    #[test]
    fn unset_and_sections() {
        let text = "[a]\n\tx = 1\n\ty = 2\n[b]\n\tz = 3\n";
        let out = apply(
            text,
            &Mutation::Unset {
                key: "a.x".to_string(),
                value_pattern: None,
                all: false,
            },
        )
        .unwrap();
        assert_eq!(out, "[a]\n\ty = 2\n[b]\n\tz = 3\n");

        let out = apply(
            out.as_str(),
            &Mutation::RemoveSection {
                name: "b".to_string(),
            },
        )
        .unwrap();
        assert_eq!(out, "[a]\n\ty = 2\n");

        let out = apply(
            out.as_str(),
            &Mutation::RenameSection {
                old: "a".to_string(),
                new: "renamed".to_string(),
            },
        )
        .unwrap();
        assert_eq!(out, "[renamed]\n\ty = 2\n");
    }

    #[test]
    fn multiple_values_require_replace_all() {
        let text = "[m]\n\tk = 1\n\tk = 2\n";
        let err = apply(text, &set("m.k", "3")).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        let out = apply(
            text,
            &Mutation::Set {
                key: "m.k".to_string(),
                value: "3".to_string(),
                value_pattern: None,
                replace_all: true,
                add: false,
            },
        )
        .unwrap();
        assert_eq!(out, "[m]\n\tk = 3\n");
    }
}
