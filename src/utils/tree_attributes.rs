//! Attribute matching for committed tree snapshots.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};

pub const GIT_ATTRIBUTES_FILE: &str = ".gitattributes";
pub const LIBRA_ATTRIBUTES_FILE: &str = ".libra_attributes";

/// One attributes file loaded from a committed tree.
pub struct TreeAttributeSource {
    pub path: PathBuf,
    pub contents: Vec<u8>,
}

/// Matcher for `export-ignore` rules in tree attributes.
pub struct ExportIgnoreMatcher {
    rules_by_dir: BTreeMap<PathBuf, Vec<ExportIgnoreRule>>,
}

struct ExportIgnoreRule {
    matcher: Gitignore,
    ignored: bool,
}

impl ExportIgnoreMatcher {
    pub fn from_sources(sources: &[TreeAttributeSource]) -> Self {
        let mut sorted = sources.iter().collect::<Vec<_>>();
        sorted.sort_by(|left, right| {
            source_dir(&left.path)
                .cmp(&source_dir(&right.path))
                .then_with(|| source_rank(&left.path).cmp(&source_rank(&right.path)))
                .then_with(|| left.path.cmp(&right.path))
        });

        let mut rules_by_dir: BTreeMap<PathBuf, Vec<ExportIgnoreRule>> = BTreeMap::new();
        for source in sorted {
            if !is_tree_attribute_file(&source.path) {
                continue;
            }
            let dir = source_dir(&source.path);
            let base = synthetic_base(&dir);
            for line in String::from_utf8_lossy(&source.contents).lines() {
                let Some((pattern, ignored)) = parse_export_ignore_rule(line) else {
                    continue;
                };
                let Some(matcher) = compile_attribute_pattern(&pattern, &base) else {
                    continue;
                };
                rules_by_dir
                    .entry(dir.clone())
                    .or_default()
                    .push(ExportIgnoreRule { matcher, ignored });
            }
        }

        Self { rules_by_dir }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        let absolute = synthetic_path(path);
        let mut ignored = false;
        for dir in ancestor_dirs(path) {
            let Some(rules) = self.rules_by_dir.get(&dir) else {
                continue;
            };
            for rule in rules {
                if !matches!(rule.matcher.matched(&absolute, false), Match::None) {
                    ignored = rule.ignored;
                }
            }
        }
        ignored
    }
}

pub fn is_tree_attribute_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(GIT_ATTRIBUTES_FILE | LIBRA_ATTRIBUTES_FILE)
    )
}

fn parse_export_ignore_rule(line: &str) -> Option<(String, bool)> {
    let tokens = split_attribute_line(line)?;
    if tokens.len() < 2 {
        return None;
    }

    let mut ignored = None;
    for token in &tokens[1..] {
        if token == "export-ignore" || token.starts_with("export-ignore=") {
            ignored = Some(true);
        } else if token == "-export-ignore" || token == "!export-ignore" {
            ignored = Some(false);
        }
    }
    ignored.map(|state| (tokens[0].clone(), state))
}

fn split_attribute_line(line: &str) -> Option<Vec<String>> {
    let line = line.trim_end_matches('\r');
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in trimmed.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn compile_attribute_pattern(pattern: &str, base: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(base);
    if builder.add_line(None, pattern).is_err() {
        return None;
    }
    builder.build().ok()
}

fn source_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf()
}

fn source_rank(path: &Path) -> u8 {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(GIT_ATTRIBUTES_FILE) => 0,
        Some(LIBRA_ATTRIBUTES_FILE) => 1,
        _ => 2,
    }
}

fn ancestor_dirs(path: &Path) -> Vec<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let mut dirs = vec![PathBuf::new()];
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        dirs.push(current.clone());
    }
    dirs
}

fn synthetic_root() -> PathBuf {
    std::env::temp_dir().join("libra-archive-tree-attributes")
}

fn synthetic_base(dir: &Path) -> PathBuf {
    synthetic_root().join(dir)
}

fn synthetic_path(path: &Path) -> PathBuf {
    synthetic_root().join(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(path: &str, contents: &str) -> TreeAttributeSource {
        TreeAttributeSource {
            path: PathBuf::from(path),
            contents: contents.as_bytes().to_vec(),
        }
    }

    #[test]
    fn export_ignore_uses_tree_source_precedence() {
        let matcher = ExportIgnoreMatcher::from_sources(&[
            source(".gitattributes", "*.txt export-ignore\n"),
            source("dir/.libra_attributes", "visible.txt -export-ignore\n"),
        ]);

        assert!(matcher.is_ignored(Path::new("secret.txt")));
        assert!(!matcher.is_ignored(Path::new("dir/visible.txt")));
    }
}
