//! Canonical unified-diff hunk header formatting (ADR-LG-04).
//!
//! Git prints a hunk range as `start` when the length is 1, `start,0` for an
//! empty side, and `start,length` otherwise. Before this module the patch
//! writers hard-coded `start,length`, so single-line hunks and empty sides
//! differed from Git (and `format-patch` output could not be applied by Git
//! byte-for-byte). Every patch output surface must call this one helper.

/// Format one side of a hunk header (`start` is the 1-based first line, or the
/// anchor line for an empty side; `count` is the number of lines).
pub fn format_hunk_span(start: usize, count: usize) -> String {
    match count {
        1 => start.to_string(),
        0 => format!("{start},0"),
        _ => format!("{start},{count}"),
    }
}

/// Format a complete `@@ -old +new @@` header (without a section heading).
pub fn format_hunk_header(
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
) -> String {
    format!(
        "@@ -{} +{} @@",
        format_hunk_span(old_start, old_count),
        format_hunk_span(new_start, new_count)
    )
}

/// One changed file's input to a `--dirstat` distribution. `raw_diff` is the
/// unified patch (used by the default byte-damage metric), `binary` carries the
/// `(old_size, new_size)` byte counts when the file is binary.
pub struct DirstatEntry<'a> {
    pub name: &'a str,
    pub insertions: usize,
    pub deletions: usize,
    pub raw_diff: &'a str,
    pub binary: Option<(u64, u64)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirstatMode {
    /// Git's default: byte damage (removed + added bytes).
    Changes,
    /// `lines`: added + deleted line counts (binary bytes / 64).
    Lines,
    /// `files`: each changed file contributes 1.
    Files,
}

/// Parse `--dirstat` parameters the way Git does: `changes`/`lines`/`files`,
/// `cumulative`/`noncumulative`, and a percentage cut-off (permille precision,
/// truncated after the first decimal digit). Returns the mode, cumulative flag,
/// and permille threshold.
fn parse_dirstat_params(param: &str) -> Result<(DirstatMode, bool, u64), String> {
    let mut mode = DirstatMode::Changes;
    let mut cumulative = false;
    let mut threshold: u64 = 30; // 3% default
    for part in param.split(',').filter(|part| !part.is_empty()) {
        match part {
            "changes" => mode = DirstatMode::Changes,
            "lines" => mode = DirstatMode::Lines,
            "files" => mode = DirstatMode::Files,
            "noncumulative" => cumulative = false,
            "cumulative" => cumulative = true,
            other if other.starts_with(|c: char| c.is_ascii_digit()) => {
                let mut chars = other.chars();
                let mut integer = String::new();
                while let Some(c) = chars.clone().next() {
                    if c.is_ascii_digit() {
                        integer.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let mut permille: u64 = integer
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --dirstat parameter '{other}'"))?
                    * 10;
                if let Some(rest) = other[integer.len()..].strip_prefix('.')
                    && let Some(digit) = rest.chars().next()
                    && digit.is_ascii_digit()
                {
                    permille += digit.to_digit(10).unwrap_or(0) as u64;
                }
                threshold = permille;
            }
            other => return Err(format!("unknown --dirstat parameter '{other}'")),
        }
    }
    Ok((mode, cumulative, threshold))
}

/// Byte damage Git's default `--dirstat` metric measures: removed plus added
/// bytes in the patch body. (Git derives this from its span hash; summing the
/// patch body is equivalent for ordinary edits and keeps this renderer free of
/// a second content scan.)
fn patch_byte_damage(raw_diff: &str) -> u64 {
    let mut damage = 0u64;
    for line in raw_diff.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            if !line.starts_with("diff --git") {
                damage += rest.len() as u64 + 1;
            }
        } else if let Some(rest) = line.strip_prefix('+') {
            damage += rest.len() as u64 + 1;
        }
    }
    damage
}

struct DirstatFile<'a> {
    name: &'a str,
    changed: u64,
}

/// Recursive distribution gather, mirroring Git: a directory is printed only
/// when it is not the top level and does not funnel everything through a single
/// subdirectory; `cumulative` keeps child counts in the parent total.
fn gather_dirstat(
    files: &[DirstatFile<'_>],
    cursor: &mut usize,
    changed_total: u64,
    base: &str,
    cumulative: bool,
    threshold: u64,
    out: &mut Vec<String>,
) -> u64 {
    let mut sum = 0u64;
    let mut sources = 0u32;
    while *cursor < files.len() {
        let name = files[*cursor].name;
        if !name.starts_with(base) {
            break;
        }
        let rest = &name[base.len()..];
        if let Some(slash) = rest.find('/') {
            let new_base = &name[..base.len() + slash + 1];
            let changes = gather_dirstat(
                files,
                cursor,
                changed_total,
                new_base,
                cumulative,
                threshold,
                out,
            );
            sources += 1;
            sum += changes;
        } else {
            sum += files[*cursor].changed;
            *cursor += 1;
            sources += 2;
        }
    }
    if !base.is_empty() && sources != 1 && sum > 0 {
        let permille = sum * 1000 / changed_total;
        if permille >= threshold {
            out.push(format!(
                "{:4}.{:01}% {}",
                permille / 10,
                permille % 10,
                base
            ));
            if !cumulative {
                return 0;
            }
        }
    }
    sum
}

/// Render a `--dirstat` block for `entries`, sorted by path.
pub fn format_dirstat(entries: &[DirstatEntry<'_>], param: &str) -> Result<String, String> {
    let (mode, cumulative, threshold) = parse_dirstat_params(param)?;

    let mut files: Vec<DirstatFile<'_>> = entries
        .iter()
        .map(|entry| {
            let changed = match mode {
                DirstatMode::Changes => entry.binary.map_or_else(
                    || patch_byte_damage(entry.raw_diff).max(1),
                    |(old, new)| old + new,
                ),
                DirstatMode::Lines => {
                    let damage = (entry.insertions + entry.deletions) as u64;
                    if entry.binary.is_some() {
                        damage.div_ceil(64)
                    } else {
                        damage
                    }
                }
                DirstatMode::Files => 1,
            };
            DirstatFile {
                name: entry.name,
                changed,
            }
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(b.name));

    let changed_total: u64 = files.iter().map(|file| file.changed).sum();
    if changed_total == 0 {
        return Ok(String::new());
    }

    let mut out = Vec::new();
    let mut cursor = 0usize;
    gather_dirstat(
        &files,
        &mut cursor,
        changed_total,
        "",
        cumulative,
        threshold,
        &mut out,
    );
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_spans_match_git() {
        // New one-line file: `@@ -0,0 +1 @@`.
        assert_eq!(format_hunk_header(0, 0, 1, 1), "@@ -0,0 +1 @@");
        // New three-line file: `@@ -0,0 +1,3 @@`.
        assert_eq!(format_hunk_header(0, 0, 1, 3), "@@ -0,0 +1,3 @@");
        // Single-line modification: `@@ -1 +1 @@`.
        assert_eq!(format_hunk_header(1, 1, 1, 1), "@@ -1 +1 @@");
        // Two lines to one: `@@ -1,2 +1 @@`.
        assert_eq!(format_hunk_header(1, 2, 1, 1), "@@ -1,2 +1 @@");
        // Pure insertion after old line 5: `@@ -5,0 +6 @@`.
        assert_eq!(format_hunk_header(5, 0, 6, 1), "@@ -5,0 +6 @@");
        // Emptied file: `@@ -1,5 +0,0 @@`.
        assert_eq!(format_hunk_header(1, 5, 0, 0), "@@ -1,5 +0,0 @@");
    }
}

#[cfg(test)]
mod dirstat_tests {
    use super::*;

    #[test]
    fn dirstat_changes_uses_byte_damage() {
        let entries = [
            DirstatEntry {
                name: "dir1/sub/x",
                insertions: 3,
                deletions: 0,
                raw_diff: "@@ -1,3 +1,6 @@\n a\n b\n c\n+d\n+e\n+f\n",
                binary: None,
            },
            DirstatEntry {
                name: "dir1/new.txt",
                insertions: 2,
                deletions: 1,
                raw_diff: "@@ -1 +1,2 @@\n-old\n+a\n+b\n",
                binary: None,
            },
            DirstatEntry {
                name: "dir2/z",
                insertions: 1,
                deletions: 0,
                raw_diff: "@@ -2,0 +3 @@\n+c\n",
                binary: None,
            },
        ];
        let out = format_dirstat(&entries, "").unwrap();
        assert_eq!(out, "  54.5% dir1/sub/\n  36.3% dir1/\n   9.0% dir2/");
    }

    #[test]
    fn dirstat_files_and_cumulative_modes() {
        let entries = [
            DirstatEntry {
                name: "dir1/sub/x",
                insertions: 3,
                deletions: 0,
                raw_diff: "",
                binary: None,
            },
            DirstatEntry {
                name: "dir1/sub/y",
                insertions: 3,
                deletions: 0,
                raw_diff: "",
                binary: None,
            },
            DirstatEntry {
                name: "dir1/new.txt",
                insertions: 2,
                deletions: 1,
                raw_diff: "",
                binary: None,
            },
            DirstatEntry {
                name: "dir2/z",
                insertions: 1,
                deletions: 0,
                raw_diff: "",
                binary: None,
            },
        ];
        assert_eq!(
            format_dirstat(&entries, "files,0").unwrap(),
            "  50.0% dir1/sub/\n  25.0% dir1/\n  25.0% dir2/"
        );
        assert_eq!(
            format_dirstat(&entries, "files,cumulative").unwrap(),
            "  50.0% dir1/sub/\n  75.0% dir1/\n  25.0% dir2/"
        );
    }

    #[test]
    fn dirstat_rejects_bad_parameters() {
        assert!(format_dirstat(&[], "bogus").is_err());
    }
}
