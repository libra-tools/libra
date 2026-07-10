//! Shared Git-style pathspec parsing and matching.
//!
//! This module intentionally covers the pathspec surface that is common across
//! read-only command filters first: plain path/directory prefixes, wildcard
//! patterns, and the high-value magic words `top`, `exclude`, `icase`,
//! `literal`, and `glob`.

use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use regex::{Regex, RegexBuilder};

use crate::utils::util;

#[derive(Debug, thiserror::Error)]
pub enum PathspecError {
    #[error("unsupported pathspec magic '{magic}' in '{spec}'")]
    UnsupportedMagic { spec: String, magic: String },
    #[error("pathspec '{spec}' is outside repository at '{workdir}'")]
    OutsideRepository { spec: String, workdir: PathBuf },
    #[error("invalid pathspec pattern '{spec}': {detail}")]
    InvalidPattern { spec: String, detail: String },
}

#[derive(Debug, Clone)]
pub struct PathspecSet {
    specs: Vec<Pathspec>,
    has_positive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathspecDepthRoot {
    path: PathBuf,
    icase: bool,
}

#[derive(Debug, Clone)]
struct Pathspec {
    raw: String,
    normalized: String,
    matcher: PathMatcher,
    exclude: bool,
    icase: bool,
}

#[derive(Debug, Clone)]
enum PathMatcher {
    Prefix {
        pattern: String,
        icase: bool,
    },
    Regex {
        pattern: String,
        regex: Regex,
        icase: bool,
    },
}

#[derive(Debug, Clone, Copy, Default)]
struct Magic {
    top: bool,
    exclude: bool,
    icase: bool,
    literal: bool,
    glob: bool,
}

impl PathspecSet {
    pub fn from_workdir(
        raw_specs: &[String],
        current_dir: &Path,
        workdir: &Path,
    ) -> Result<Self, PathspecError> {
        Self::from_workdir_with_default_icase(raw_specs, current_dir, workdir, false)
    }

    pub fn from_workdir_with_default_icase(
        raw_specs: &[String],
        current_dir: &Path,
        workdir: &Path,
        default_icase: bool,
    ) -> Result<Self, PathspecError> {
        let specs = raw_specs
            .iter()
            .map(|raw| Pathspec::parse(raw, current_dir, workdir, default_icase))
            .collect::<Result<Vec<_>, _>>()?;
        let has_positive = specs.iter().any(|spec| !spec.exclude);
        Ok(Self {
            specs,
            has_positive,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    pub fn matches_path(&self, path: impl AsRef<Path>) -> bool {
        if self.specs.is_empty() {
            return true;
        }

        let path = normalize_candidate(path.as_ref());
        let positive = if self.has_positive {
            self.specs
                .iter()
                .any(|spec| !spec.exclude && spec.matcher.matches(&path))
        } else {
            true
        };
        positive
            && !self
                .specs
                .iter()
                .any(|spec| spec.exclude && spec.matcher.matches(&path))
    }

    pub fn unmatched_positive<I, P>(&self, paths: I) -> Option<&str>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let normalized = paths
            .into_iter()
            .map(|path| normalize_candidate(path.as_ref()))
            .collect::<Vec<_>>();
        self.specs
            .iter()
            .filter(|spec| !spec.exclude)
            .find(|spec| {
                !normalized
                    .iter()
                    .any(|path| spec.matcher.matches(path.as_str()))
            })
            .map(|spec| spec.raw.as_str())
    }

    pub fn unmatched_positive_specs<I, P>(&self, paths: I) -> Vec<&str>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let normalized = paths
            .into_iter()
            .map(|path| normalize_candidate(path.as_ref()))
            .collect::<Vec<_>>();
        self.specs
            .iter()
            .filter(|spec| !spec.exclude)
            .filter(|spec| {
                !normalized
                    .iter()
                    .any(|path| spec.matcher.matches(path.as_str()))
            })
            .map(|spec| spec.raw.as_str())
            .collect()
    }

    /// Return plain positive prefix pathspecs that can be passed to older
    /// command engines as a pre-filter without changing behavior.
    pub fn plain_positive_prefixes(&self) -> Option<Vec<PathBuf>> {
        if self.specs.iter().any(|spec| spec.exclude) {
            return None;
        }
        let mut prefixes = Vec::new();
        for spec in &self.specs {
            match &spec.matcher {
                PathMatcher::Prefix { pattern, icase } if !icase => {
                    prefixes.push(if pattern.is_empty() {
                        PathBuf::from(".")
                    } else {
                        PathBuf::from(pattern)
                    });
                }
                _ => return None,
            }
        }
        Some(prefixes)
    }

    pub fn positive_prefixes(&self) -> Vec<PathBuf> {
        self.specs
            .iter()
            .filter(|spec| !spec.exclude)
            .filter_map(|spec| match &spec.matcher {
                PathMatcher::Prefix { pattern, .. } => Some(if pattern.is_empty() {
                    PathBuf::from(".")
                } else {
                    PathBuf::from(pattern)
                }),
                PathMatcher::Regex { .. } => None,
            })
            .collect()
    }

    pub fn positive_depth_roots(&self) -> Vec<PathspecDepthRoot> {
        self.specs
            .iter()
            .filter(|spec| !spec.exclude)
            .map(Pathspec::depth_root)
            .collect()
    }
}

impl PathspecDepthRoot {
    pub fn case_sensitive(path: PathBuf) -> Self {
        Self { path, icase: false }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn icase(&self) -> bool {
        self.icase
    }
}

impl Pathspec {
    fn parse(
        raw: &str,
        current_dir: &Path,
        workdir: &Path,
        default_icase: bool,
    ) -> Result<Self, PathspecError> {
        let (magic, body) = parse_magic(raw)?;
        let normalized = resolve_body(raw, body, magic.top, current_dir, workdir)?;
        let icase = magic.icase || default_icase;
        let matcher = if magic.literal || !has_wildcard(&normalized) {
            PathMatcher::Prefix {
                pattern: normalized.clone(),
                icase,
            }
        } else {
            PathMatcher::Regex {
                pattern: normalized.clone(),
                regex: compile_pattern(raw, &normalized, magic.glob, icase)?,
                icase,
            }
        };
        Ok(Self {
            raw: raw.to_string(),
            normalized,
            matcher,
            exclude: magic.exclude,
            icase,
        })
    }

    fn depth_root(&self) -> PathspecDepthRoot {
        let path = match &self.matcher {
            PathMatcher::Prefix { pattern, .. } => PathBuf::from(pattern),
            PathMatcher::Regex { .. } => wildcard_base(&self.normalized),
        };
        PathspecDepthRoot {
            path,
            icase: self.icase,
        }
    }
}

impl PathMatcher {
    fn matches(&self, path: &str) -> bool {
        match self {
            Self::Prefix { pattern, icase } => prefix_matches(pattern, path, *icase),
            Self::Regex {
                pattern,
                regex,
                icase,
            } => prefix_matches(pattern, path, *icase) || regex.is_match(path),
        }
    }
}

fn parse_magic(raw: &str) -> Result<(Magic, &str), PathspecError> {
    let Some(rest) = raw.strip_prefix(':') else {
        return Ok((Magic::default(), raw));
    };

    if let Some(after_top) = rest.strip_prefix('/') {
        let magic = Magic {
            top: true,
            ..Magic::default()
        };
        return Ok((magic, after_top));
    }

    if let Some(after_exclude) = rest.strip_prefix('!').or_else(|| rest.strip_prefix('^')) {
        let magic = Magic {
            exclude: true,
            ..Magic::default()
        };
        return Ok((magic, after_exclude));
    }

    let Some(long) = rest.strip_prefix('(') else {
        return Ok((Magic::default(), raw));
    };
    let Some(close) = long.find(')') else {
        return Err(PathspecError::InvalidPattern {
            spec: raw.to_string(),
            detail: "missing ')' after pathspec magic".to_string(),
        });
    };

    let mut magic = Magic::default();
    let magic_words = &long[..close];
    if !magic_words.is_empty() {
        for word in magic_words.split(',') {
            match word {
                "top" => magic.top = true,
                "exclude" => magic.exclude = true,
                "icase" => magic.icase = true,
                "literal" => magic.literal = true,
                "glob" => magic.glob = true,
                other => {
                    return Err(PathspecError::UnsupportedMagic {
                        spec: raw.to_string(),
                        magic: other.to_string(),
                    });
                }
            }
        }
    }

    Ok((magic, &long[close + 1..]))
}

fn resolve_body(
    raw: &str,
    body: &str,
    top: bool,
    current_dir: &Path,
    workdir: &Path,
) -> Result<String, PathspecError> {
    let body_path = Path::new(body);
    let base = if top { workdir } else { current_dir };
    let absolute = if body_path.is_absolute() {
        body_path.to_path_buf()
    } else {
        base.join(body_path)
    };
    let normalized = normalize_lexical(&absolute);
    if !util::is_sub_path(&normalized, workdir) {
        return Err(PathspecError::OutsideRepository {
            spec: raw.to_string(),
            workdir: workdir.to_path_buf(),
        });
    }
    let relative = pathdiff::diff_paths(&normalized, workdir).ok_or_else(|| {
        PathspecError::InvalidPattern {
            spec: raw.to_string(),
            detail: "failed to relativize pathspec to repository root".to_string(),
        }
    })?;
    Ok(normalize_pattern(relative))
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new(component.as_os_str())),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn normalize_pattern(path: PathBuf) -> String {
    let mut text = path_to_slash_string(&path);
    while let Some(stripped) = text.strip_prefix("./") {
        text = stripped.to_string();
    }
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    if text == "." { String::new() } else { text }
}

fn normalize_candidate(path: &Path) -> String {
    normalize_pattern(path.to_path_buf())
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(os_to_string(part)),
            Component::CurDir => None,
            Component::ParentDir => Some("..".to_string()),
            Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn os_to_string(value: &OsStr) -> String {
    value.to_string_lossy().replace('\\', "/")
}

fn has_wildcard(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

fn wildcard_base(pattern: &str) -> PathBuf {
    let mut base = PathBuf::new();
    for component in Path::new(pattern).components() {
        match component {
            Component::Normal(part) if !has_wildcard(&os_to_string(part)) => base.push(part),
            Component::CurDir => {}
            _ => break,
        }
    }
    base
}

fn prefix_matches(pattern: &str, path: &str, icase: bool) -> bool {
    if pattern.is_empty() {
        return true;
    }
    if icase {
        let pattern = pattern.to_lowercase();
        let path = path.to_lowercase();
        return path == pattern
            || path
                .strip_prefix(pattern.as_str())
                .is_some_and(|rest| rest.starts_with('/'));
    }
    path == pattern
        || path
            .strip_prefix(pattern)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn compile_pattern(
    raw: &str,
    pattern: &str,
    glob: bool,
    icase: bool,
) -> Result<Regex, PathspecError> {
    let regex = wildcard_to_regex(pattern, glob);
    RegexBuilder::new(&regex)
        .case_insensitive(icase)
        .build()
        .map_err(|error| PathspecError::InvalidPattern {
            spec: raw.to_string(),
            detail: error.to_string(),
        })
}

fn wildcard_to_regex(pattern: &str, glob: bool) -> String {
    let mut out = String::from("^");
    let chars = pattern.chars().collect::<Vec<_>>();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if glob {
                    if chars.get(i + 1) == Some(&'*') {
                        out.push_str(".*");
                        i += 2;
                    } else {
                        out.push_str("[^/]*");
                        i += 1;
                    }
                } else {
                    out.push_str(".*");
                    i += 1;
                }
            }
            '?' => {
                out.push_str(if glob { "[^/]" } else { "." });
                i += 1;
            }
            '[' => {
                let (class, consumed) = char_class(&chars[i..]);
                out.push_str(&class);
                i += consumed;
            }
            ch => {
                out.push_str(&regex::escape(&ch.to_string()));
                i += 1;
            }
        }
    }
    out.push('$');
    out
}

fn char_class(chars: &[char]) -> (String, usize) {
    let Some(close) = chars.iter().skip(1).position(|ch| *ch == ']') else {
        return (regex::escape("["), 1);
    };
    let close = close + 1;
    let mut class = String::from("[");
    for (idx, ch) in chars[1..close].iter().enumerate() {
        if idx == 0 && matches!(*ch, '!' | '^') {
            class.push('^');
        } else if *ch == '\\' {
            class.push_str("\\\\");
        } else {
            class.push(*ch);
        }
    }
    class.push(']');
    (class, close + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(raw: &[&str], cwd: &str) -> PathspecSet {
        let workdir = Path::new("/repo");
        let cwd = workdir.join(cwd);
        PathspecSet::from_workdir(
            &raw.iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>(),
            &cwd,
            workdir,
        )
        .expect("pathspec compiles")
    }

    #[test]
    fn plain_pathspec_matches_directory_prefix() {
        let specs = set(&["src"], "");
        assert!(specs.matches_path("src/main.rs"));
        assert!(specs.matches_path("src"));
        assert!(!specs.matches_path("srcfoo/main.rs"));
    }

    #[test]
    fn top_magic_ignores_current_directory() {
        let specs = set(&[":(top)README.md"], "src");
        assert!(specs.matches_path("README.md"));
        assert!(!specs.matches_path("src/README.md"));
    }

    #[test]
    fn exclude_magic_removes_matches_after_positive_selection() {
        let specs = set(&["src", ":(exclude)src/generated.rs"], "");
        assert!(specs.matches_path("src/main.rs"));
        assert!(!specs.matches_path("src/generated.rs"));
    }

    #[test]
    fn icase_and_literal_magic_are_honored() {
        let icase = set(&[":(icase)src/readme.md"], "");
        assert!(icase.matches_path("SRC/README.MD"));

        let literal = set(&[":(literal)*.rs"], "");
        assert!(literal.matches_path("*.rs"));
        assert!(!literal.matches_path("src/main.rs"));
    }

    #[test]
    fn default_wildcard_crosses_slash_but_glob_wildcard_does_not() {
        let default = set(&["*.rs"], "");
        assert!(default.matches_path("src/main.rs"));

        let glob = set(&[":(glob)*.rs"], "");
        assert!(!glob.matches_path("src/main.rs"));
        assert!(glob.matches_path("main.rs"));
    }

    #[test]
    fn wildcard_pathspecs_also_match_exact_metachar_paths() {
        let specs = set(&["literal/[abc].txt"], "");
        assert!(specs.matches_path("literal/[abc].txt"));
        assert!(specs.matches_path("literal/a.txt"));
        assert!(!specs.matches_path("literal/d.txt"));

        let directory = set(&["literal/[abc]"], "");
        assert!(directory.matches_path("literal/[abc]/child.txt"));
        assert!(directory.matches_path("literal/a"));
    }

    #[test]
    fn relative_specs_are_resolved_from_cwd() {
        let specs = set(&["*.rs"], "src");
        assert!(specs.matches_path("src/main.rs"));
        assert!(!specs.matches_path("main.rs"));
    }

    #[test]
    fn positive_depth_roots_use_fixed_prefix_before_wildcards() {
        let specs = set(&[":(glob)src/*.rs", ":(exclude)src/generated.rs"], "");
        assert_eq!(
            specs.positive_depth_roots(),
            vec![PathspecDepthRoot::case_sensitive(PathBuf::from("src"))]
        );

        let root_glob = set(&["*.rs"], "");
        assert_eq!(
            root_glob.positive_depth_roots(),
            vec![PathspecDepthRoot::case_sensitive(PathBuf::new())]
        );
    }

    #[test]
    fn positive_depth_roots_preserve_icase_magic() {
        let specs = set(&[":(icase)src/case.txt"], "");
        assert_eq!(
            specs.positive_depth_roots(),
            vec![PathspecDepthRoot {
                path: PathBuf::from("src/case.txt"),
                icase: true,
            }]
        );
    }
}
