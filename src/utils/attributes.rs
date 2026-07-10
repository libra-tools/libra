//! Shared Git/Libra attributes source resolution and matching.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};
use once_cell::sync::Lazy;

use crate::utils::util;

const LIBRA_ATTRIBUTES_FILE: &str = ".libra_attributes";
const GIT_ATTRIBUTES_FILE: &str = ".gitattributes";
const CORE_ATTRIBUTES_FILE_KEY: &str = "core.attributesFile";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeState {
    Set,
    Value(String),
    Unset,
    Unspecified,
}

impl AttributeState {
    pub fn check_attr_value(&self) -> Option<String> {
        match self {
            Self::Set => Some("set".to_string()),
            Self::Value(value) => Some(value.clone()),
            Self::Unset => Some("unset".to_string()),
            Self::Unspecified => None,
        }
    }
}

#[derive(Debug, Clone)]
struct AttributeAssignment {
    name: String,
    state: AttributeState,
}

struct AttributeRule {
    matcher: Gitignore,
    assignments: Vec<AttributeAssignment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AttributeCacheKey {
    source: PathBuf,
    base: PathBuf,
}

struct CachedAttributes {
    len: u64,
    modified: SystemTime,
    rules: Arc<Vec<AttributeRule>>,
}

struct AttributeSource {
    path: PathBuf,
    base: PathBuf,
}

static ATTRIBUTES_CACHE: Lazy<Mutex<HashMap<AttributeCacheKey, CachedAttributes>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn attribute_state_for_path(attr: &str, path: &Path) -> Option<AttributeState> {
    let workdir = util::working_dir();
    let absolute = absolute_in_workdir(path, &workdir)?;
    let mut state = None;
    for source in attribute_sources_for_path(&workdir, &absolute) {
        for rule in cached_attribute_file(&source.path, &source.base).iter() {
            if !attribute_rule_matches(rule, &absolute) {
                continue;
            }
            for assignment in &rule.assignments {
                if assignment.name == attr {
                    state = Some(assignment.state.clone());
                }
            }
        }
    }
    state.and_then(|value| match value {
        AttributeState::Unspecified => None,
        other => Some(other),
    })
}

pub fn all_attribute_states_for_path(path: &Path) -> BTreeMap<String, AttributeState> {
    let workdir = util::working_dir();
    let Some(absolute) = absolute_in_workdir(path, &workdir) else {
        return BTreeMap::new();
    };
    let mut states = BTreeMap::new();
    for source in attribute_sources_for_path(&workdir, &absolute) {
        for rule in cached_attribute_file(&source.path, &source.base).iter() {
            if !attribute_rule_matches(rule, &absolute) {
                continue;
            }
            for assignment in &rule.assignments {
                if matches!(assignment.state, AttributeState::Unspecified) {
                    states.remove(&assignment.name);
                } else {
                    states.insert(assignment.name.clone(), assignment.state.clone());
                }
            }
        }
    }
    states
}

pub fn is_lfs_tracked(path: &Path) -> bool {
    matches!(
        attribute_state_for_path("filter", path),
        Some(AttributeState::Value(value)) if value == "lfs"
    )
}

pub fn diff_driver_for_path(path: &Path) -> Option<String> {
    match attribute_state_for_path("diff", path) {
        Some(AttributeState::Value(driver)) if !driver.is_empty() => Some(driver),
        _ => None,
    }
}

pub fn is_export_ignored(path: &Path) -> bool {
    matches!(
        attribute_state_for_path("export-ignore", path),
        Some(AttributeState::Set | AttributeState::Value(_))
    )
}

fn absolute_in_workdir(path: &Path, workdir: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    };
    util::is_sub_path(&absolute, workdir).then_some(absolute)
}

fn attribute_sources_for_path(workdir: &Path, absolute: &Path) -> Vec<AttributeSource> {
    let mut sources = Vec::new();
    if let Some(configured) = util::optional_cascaded_config_path(CORE_ATTRIBUTES_FILE_KEY, workdir)
    {
        push_attribute_source(&mut sources, configured, workdir.to_path_buf());
    }
    for dir in attribute_dirs(workdir, absolute) {
        push_attribute_source(&mut sources, dir.join(GIT_ATTRIBUTES_FILE), dir.clone());
        push_attribute_source(&mut sources, dir.join(LIBRA_ATTRIBUTES_FILE), dir);
    }
    if let Some(info_attributes) = util::git_info_file_path(workdir, "attributes") {
        push_attribute_source(&mut sources, info_attributes, workdir.to_path_buf());
    }
    sources
}

fn attribute_dirs(workdir: &Path, absolute: &Path) -> Vec<PathBuf> {
    let parent = absolute.parent().unwrap_or(workdir);
    let Ok(relative_parent) = parent.strip_prefix(workdir) else {
        return vec![workdir.to_path_buf()];
    };
    let mut dirs = vec![workdir.to_path_buf()];
    let mut current = workdir.to_path_buf();
    for component in relative_parent.components() {
        current.push(component.as_os_str());
        dirs.push(current.clone());
    }
    dirs
}

fn push_attribute_source(sources: &mut Vec<AttributeSource>, path: PathBuf, base: PathBuf) {
    if path.exists() {
        sources.push(AttributeSource { path, base });
    }
}

fn cached_attribute_file(path: &Path, base: &Path) -> Arc<Vec<AttributeRule>> {
    let Ok(metadata) = fs::metadata(path) else {
        return Arc::new(Vec::new());
    };
    let Ok(modified) = metadata.modified() else {
        return Arc::new(parse_attribute_file(path, base));
    };
    let len = metadata.len();
    let key = AttributeCacheKey {
        source: path.to_path_buf(),
        base: base.to_path_buf(),
    };
    let mut cache = match ATTRIBUTES_CACHE.lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cached) = cache.get(&key)
        && cached.len == len
        && cached.modified == modified
    {
        return Arc::clone(&cached.rules);
    }

    let rules = Arc::new(parse_attribute_file(path, base));
    cache.insert(
        key,
        CachedAttributes {
            len,
            modified,
            rules: Arc::clone(&rules),
        },
    );
    rules
}

fn parse_attribute_file(path: &Path, base: &Path) -> Vec<AttributeRule> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_attribute_contents(&contents, base)
}

fn parse_attribute_contents(contents: &str, base: &Path) -> Vec<AttributeRule> {
    let mut rules = Vec::new();
    for line in contents.lines() {
        let Some(tokens) = split_attribute_line(line) else {
            continue;
        };
        if tokens.len() < 2 {
            continue;
        }
        let pattern = &tokens[0];
        let assignments = tokens[1..]
            .iter()
            .filter_map(|token| parse_assignment(token))
            .collect::<Vec<_>>();
        if assignments.is_empty() {
            continue;
        }
        if let Some(matcher) = compile_attribute_pattern(pattern, base) {
            rules.push(AttributeRule {
                matcher,
                assignments,
            });
        }
    }
    rules
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

fn parse_assignment(token: &str) -> Option<AttributeAssignment> {
    let (name, state) = if let Some(name) = token.strip_prefix('-') {
        (name, AttributeState::Unset)
    } else if let Some(name) = token.strip_prefix('!') {
        (name, AttributeState::Unspecified)
    } else if let Some((name, value)) = token.split_once('=') {
        (name, AttributeState::Value(value.to_string()))
    } else {
        (token, AttributeState::Set)
    };
    (!name.is_empty()).then(|| AttributeAssignment {
        name: name.to_string(),
        state,
    })
}

fn compile_attribute_pattern(pattern: &str, base: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(base);
    if builder.add_line(None, pattern).is_err() {
        return None;
    }
    builder.build().ok()
}

fn attribute_rule_matches(rule: &AttributeRule, path: &Path) -> bool {
    !matches!(rule.matcher.matched(path, path.is_dir()), Match::None)
}
