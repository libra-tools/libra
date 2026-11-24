use super::describe_types::DescribeOutput;

pub(super) fn describe_output(
    input: String,
    resolved_commit: String,
    tag_name: &str,
    distance: usize,
    abbrev: usize,
    long_format: bool,
    force_suffix: bool,
) -> DescribeOutput {
    // `--abbrev=0` still renders the forced `<tag>-<distance>-g<oid>` suffix,
    // with the full object id (Git's `repo_find_unique_abbrev(oid, 0)`).
    let abbreviated_commit = (long_format || distance > 0 || force_suffix)
        .then(|| abbreviate_hash(&resolved_commit, abbrev));
    DescribeOutput {
        input,
        resolved_commit: resolved_commit.clone(),
        result: format_describe_result(
            tag_name,
            distance,
            &resolved_commit,
            abbrev,
            long_format,
            force_suffix,
        ),
        tag: Some(tag_name.to_string()),
        distance: Some(distance),
        abbreviated_commit,
        exact_match: distance == 0,
        used_always: false,
        long_format,
        dirty: false,
        dirty_mark: None,
    }
}

pub(super) fn abbreviate_hash(full_sha: &str, abbrev: usize) -> String {
    if abbrev == 0 || abbrev >= full_sha.len() {
        full_sha.to_string()
    } else {
        full_sha[..abbrev].to_string()
    }
}

fn format_describe_result(
    tag_name: &str,
    distance: usize,
    full_sha: &str,
    abbrev: usize,
    long_format: bool,
    force_suffix: bool,
) -> String {
    if distance == 0 && !long_format && !force_suffix {
        tag_name.to_string()
    } else {
        let short_sha = abbreviate_hash(full_sha, abbrev);
        format!("{tag_name}-{distance}-g{short_sha}")
    }
}
