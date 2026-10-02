//! `libra mega2 browser` — the single public Mega2 surface (plan-20260912 MB-03).
//!
//! The command is a thin adapter over two already-verified layers; it reads
//! by default and sends one remote write request only for a confirmed TUI
//! action or a non-interactive write flag such as `--create-dir`:
//! the bounded transport in [`crate::internal::protocol::mega2_tree`] (MB-01)
//! and the resumable terminal state in [`crate::command::mega2_browser`]
//! (MB-02). It never opens the repository database, never touches the index or
//! object store, and never persists configuration — everything it needs comes
//! from argv and the remote server.
//!
//! Three paths share the same validated inputs:
//!
//! - human/TUI (no operation flag): exactly the MB-02 interactive loop, which
//!   itself requires stdin and stdout to be TTYs before altering any terminal
//!   state;
//! - non-interactive human (an operation flag such as `--list`): one request
//!   through [`crate::command::mega2_browser::noninteractive`], printed as
//!   sanitized plain text, with no terminal and no stdin;
//! - `--json`/`--machine`: the same non-interactive path, rendered through the
//!   shared JSON envelope; without an operation flag it lists PATH, exactly
//!   like `--list`.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::{
    command::mega2_browser::noninteractive::{self, Invocation, Operation},
    internal::protocol::{
        mega2_auth::resolve_token_from_process,
        mega2_tree::{ContentType, Listing, normalize_path, validate_server_url},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
    },
};

/// `EXAMPLES:` banner for the `mega2` parent command.
pub const MEGA2_EXAMPLES: &str = "\
EXAMPLES:
    libra mega2 browser --server https://mega2.example.com         Browse the remote root in the TUI
    libra mega2 browser --server https://mega2.example.com /src    Open a rooted path directly
    libra mega2 browser --server https://mega2.example.com --list /src  Print one level, no terminal needed
    libra --json mega2 browser --server http://127.0.0.1:8080      One bounded fetch as JSON
    libra --machine mega2 browser --server https://mega2.example.com  Strict machine mode

`mega2 browser` is Libra-only: it lists one remote directory level and has no
Git-equivalent contract. `libra ls-tree` inspects local tree objects instead.";

/// `EXAMPLES:` banner for `libra mega2 browser`.
pub const MEGA2_BROWSER_EXAMPLES: &str = "\
EXAMPLES:
    libra mega2 browser --server https://mega2.example.com             Interactive browse of /
    libra mega2 browser --server https://mega2.example.com /src/pkg    Interactive browse of /src/pkg
    libra mega2 browser --server https://mega2.example.com --ref v1.2  List one commit or tag
    libra mega2 browser --server https://mega2.example.com --list /src/pkg  Plain-text listing, no terminal
    libra mega2 browser --server http://127.0.0.1:8080 --json          Exactly one fetch, JSON schema
    libra --machine mega2 browser --server https://mega2.example.com   NDJSON for automation
    libra mega2 browser --server https://mega2.example.com --token-file ~/.mega2-token  Create/delete/move with a token file

Keys (interactive mode): Up/Down or k/j select, Enter opens a directory,
Backspace or h goes to the parent, + creates a directory, d deletes the
selected directory (after a confirmation line), m moves it, R renames it
(same parent), r reloads, q quits.

Write token precedence: --token-file, then LIBRA_MEGA2_TOKEN, then --token
(warned: --token is visible in shell history). Tokens are never echoed.";

/// `libra mega2 <subcommand>`; the parent exposes exactly one child.
#[derive(Args, Debug)]
pub struct Mega2Args {
    #[command(subcommand)]
    pub command: Mega2Subcommand,
}

/// The only registered `mega2` subcommand.
#[derive(Subcommand, Debug)]
pub enum Mega2Subcommand {
    #[command(
        about = "Browse a remote Mega2 directory in a TUI, or run one non-interactive operation (--list, --json)",
        after_help = MEGA2_BROWSER_EXAMPLES
    )]
    Browser(BrowserArgs),
}

/// Arguments for the single public browser surface.
#[derive(Args, Debug)]
pub struct BrowserArgs {
    /// Mega2 server base URL (https://host, or loopback http://host:port)
    #[arg(long, value_name = "BASE-URL")]
    pub server: String,

    /// Rooted directory: the one to list, or the parent for a directory write; never escapes above `/`
    #[arg(value_name = "PATH", default_value = "/")]
    pub path: String,

    /// Optional commit or tag to list instead of the server default branch
    #[arg(long = "ref", value_name = "COMMIT-OR-TAG")]
    pub git_ref: Option<String>,

    /// Read the write token from this file (highest precedence)
    #[arg(long = "token-file", value_name = "PATH")]
    pub token_file: Option<PathBuf>,

    /// Write token inline (lowest precedence; visible in shell history — prefer --token-file)
    #[arg(long = "token", value_name = "TOKEN")]
    pub token: Option<String>,

    #[command(flatten)]
    pub operation: OperationArgs,
}

/// Non-interactive operations (plan-20261001 ADR-MN-01): at most one per
/// invocation, each sending exactly one request without a terminal.
#[derive(Args, Debug, Default)]
#[group(id = "operation", multiple = false)]
pub struct OperationArgs {
    /// List PATH once and exit: one GET, plain text (or JSON with --json); no terminal needed
    #[arg(long)]
    pub list: bool,

    /// Create directory NAME under PATH: one POST, no reload
    #[arg(long = "create-dir", value_name = "NAME")]
    pub create_dir: Option<String>,

    /// Delete directory NAME under PATH: one POST, no confirmation, no reload
    #[arg(long = "delete-dir", value_name = "NAME")]
    pub delete_dir: Option<String>,

    /// Move directory NAME from PATH into PARENT-PATH, keeping its name: one POST, no reload
    #[arg(long = "move-dir", num_args = 2, value_names = ["NAME", "PARENT-PATH"])]
    pub move_dir: Option<Vec<String>>,
}

impl OperationArgs {
    /// The selected operation, if any.
    fn selected(&self) -> CliResult<Option<Operation>> {
        if self.list {
            return Ok(Some(Operation::List));
        }
        if let Some(name) = &self.create_dir {
            return Ok(Some(Operation::CreateDir { name: name.clone() }));
        }
        if let Some(name) = &self.delete_dir {
            return Ok(Some(Operation::DeleteDir { name: name.clone() }));
        }
        match self.move_dir.as_deref() {
            None => Ok(None),
            Some([name, to_parent]) => Ok(Some(Operation::MoveDir {
                name: name.clone(),
                to_parent: to_parent.clone(),
            })),
            // clap enforces `num_args = 2`; any other arity is a parser bug.
            Some(values) => Err(CliError::internal(format!(
                "mega2 browser: --move-dir expects 2 values, got {}",
                values.len()
            ))),
        }
    }
}

/// One validated listing entry in the documented machine schema.
#[derive(Serialize, Debug, PartialEq, Eq)]
struct BrowserItem<'a> {
    name: &'a str,
    content_type: &'a str,
}

/// The documented `mega2 browser` list payload (`data` of the JSON envelope).
#[derive(Serialize, Debug)]
pub(crate) struct BrowserData<'a> {
    /// Always `list` (plan-20261001 ADR-MN-03).
    operation: &'static str,
    server: &'a str,
    #[serde(rename = "ref")]
    git_ref: Option<&'a str>,
    path: &'a str,
    items: Vec<BrowserItem<'a>>,
}

/// Canonical, credential-free rendering of a validated server URL.
///
/// `validate_server_url` already rejects userinfo, query and fragment, so the
/// serialization origin is exactly scheme + host + optional port.
fn canonical_server(url: &url::Url) -> String {
    url.origin().ascii_serialization()
}

fn content_type_name(content_type: ContentType) -> &'static str {
    match content_type {
        ContentType::Directory => "directory",
        ContentType::File => "file",
    }
}

pub(crate) fn browser_data<'a>(
    server: &'a str,
    git_ref: Option<&'a str>,
    path: &'a str,
    listing: &'a Listing,
) -> BrowserData<'a> {
    BrowserData {
        operation: Operation::List.name(),
        server,
        git_ref,
        path,
        items: listing
            .entries
            .iter()
            .map(|entry| BrowserItem {
                name: &entry.name,
                content_type: content_type_name(entry.content_type),
            })
            .collect(),
    }
}

/// # Side Effects
///
/// Reads one bounded remote listing (JSON/machine mode, or human `--list`) or
/// drives the MB-02 TUI (human mode without an operation flag). A
/// non-interactive write flag sends exactly one remote write request. It never
/// opens the repository database, object store, index or configuration. Read
/// requests carry no credentials; a write request carries at most one Bearer
/// token, taken from `--token-file` → `LIBRA_MEGA2_TOKEN` → `--token`.
///
/// # Errors
///
/// Returns structured CLI errors for invalid invocation (bad URL/path), a
/// missing terminal in human mode, unavailable network, refused/redirected
/// HTTP responses, and malformed or hostile server responses.
pub async fn execute_safe(args: Mega2Args, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        Mega2Subcommand::Browser(browser) => execute_browser(browser, output).await,
    }
}

async fn execute_browser(args: BrowserArgs, output: &OutputConfig) -> CliResult<()> {
    // Validation happens before any terminal change or network request.
    let url = validate_server_url(&args.server)?;
    let path = normalize_path(&args.path)?;
    let server = canonical_server(&url);
    let git_ref = args.git_ref.as_deref();

    // Non-interactive path: an operation flag, or a machine output mode
    // without one (which lists PATH). The registry's class rules — including
    // the refusal of token flags — run there, before the one request.
    if let Some(operation) = args
        .operation
        .selected()?
        .or_else(|| output.is_json().then_some(Operation::List))
    {
        let invocation = Invocation {
            server: &server,
            path: &path,
            git_ref,
            token_file: args.token_file.as_deref(),
            token: args.token.as_deref(),
        };
        return noninteractive::execute(operation, &invocation, output).await;
    }

    if output.quiet {
        return Err(CliError::fatal(
            "mega2 browser: --quiet needs a machine output mode; use --json or --machine",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("run `libra --machine mega2 browser --server <base-url>` for NDJSON output"));
    }

    // ADR-MB-03 precedence: --token-file → LIBRA_MEGA2_TOKEN → --token.
    let (token, _source) =
        resolve_token_from_process(args.token_file.as_deref(), args.token.as_deref())?;

    crate::command::mega2_browser::run(&server, &path, git_ref, token).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_server_drops_trailing_slash_and_keeps_port() {
        let url = validate_server_url("https://mega2.example.com/").expect("valid url");
        assert_eq!(canonical_server(&url), "https://mega2.example.com");
        let loopback = validate_server_url("http://127.0.0.1:8080").expect("valid loopback");
        assert_eq!(canonical_server(&loopback), "http://127.0.0.1:8080");
    }

    #[test]
    fn machine_payload_uses_the_documented_schema() {
        let listing = Listing {
            entries: vec![
                crate::internal::protocol::mega2_tree::ListingEntry {
                    name: "dir".to_string(),
                    content_type: ContentType::Directory,
                },
                crate::internal::protocol::mega2_tree::ListingEntry {
                    name: "file.txt".to_string(),
                    content_type: ContentType::File,
                },
            ],
        };
        let data = browser_data("https://mega2.example.com", Some("v1"), "/src", &listing);
        let json = serde_json::to_value(&data).expect("serialize");
        assert_eq!(json["server"], "https://mega2.example.com");
        assert_eq!(json["ref"], "v1");
        assert_eq!(json["path"], "/src");
        assert_eq!(json["items"][0]["name"], "dir");
        assert_eq!(json["items"][0]["content_type"], "directory");
        assert_eq!(json["items"][1]["content_type"], "file");
    }

    #[test]
    fn browser_args_parse_ref_and_default_path() {
        use clap::Parser;

        #[derive(Parser, Debug)]
        struct Probe {
            #[command(flatten)]
            args: BrowserArgs,
        }

        let parsed = Probe::try_parse_from([
            "browser",
            "--server",
            "https://mega2.example.com",
            "--ref",
            "v1",
        ])
        .expect("parse");
        assert_eq!(parsed.args.path, "/");
        assert_eq!(parsed.args.git_ref.as_deref(), Some("v1"));
    }

    // ---- plan-20261001 MN-02: list payload and rooted examples ----

    #[test]
    fn list_payload_carries_operation() {
        let listing = Listing { entries: vec![] };
        let data = browser_data("https://mega2.example.com", None, "/", &listing);
        let json = serde_json::to_value(&data).expect("serialize");
        assert_eq!(json["operation"], "list");
    }

    /// `mega2 browser` invocations in a help EXAMPLES block: the text before
    /// the description column (the first run of two spaces) of each line.
    fn help_examples(block: &str) -> Vec<String> {
        block
            .lines()
            .map(|line| line.trim().split("  ").next().unwrap_or_default())
            .filter(|command| command.starts_with("libra ") && command.contains(" mega2 browser"))
            .map(str::to_string)
            .collect()
    }

    /// `mega2 browser` invocations inside the shell code fences of a Markdown
    /// page. The bare-fence Synopsis is a grammar with placeholders, not an
    /// example, and is skipped with every other non-shell fence.
    fn markdown_examples(text: &str) -> Vec<String> {
        let mut examples = Vec::new();
        // `Some(is_shell)` while inside a fence.
        let mut fence: Option<bool> = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(info) = trimmed.strip_prefix("```") {
                fence = match fence {
                    Some(_) => None,
                    None => Some(matches!(info.trim(), "bash" | "sh" | "shell" | "console")),
                };
                continue;
            }
            let command = trimmed.strip_prefix("$ ").unwrap_or(trimmed);
            if fence == Some(true)
                && command.starts_with("libra ")
                && command.contains(" mega2 browser")
            {
                examples.push(command.to_string());
            }
        }
        examples
    }

    /// PATH of one `libra … mega2 browser …` argv, read through the real CLI.
    fn example_path(argv: &[String]) -> Option<String> {
        use clap::{CommandFactory, FromArgMatches};

        let matches = crate::cli::Cli::command().try_get_matches_from(argv).ok()?;
        let ("mega2", mega2) = matches.subcommand()? else {
            return None;
        };
        let ("browser", browser) = mega2.subcommand()? else {
            return None;
        };
        BrowserArgs::from_arg_matches(browser)
            .ok()
            .map(|args| args.path)
    }

    /// Examples that `Cli::try_parse_from` refuses or whose PATH
    /// `normalize_path` refuses, each with the reason.
    fn unrooted_examples(examples: &[String]) -> std::collections::BTreeSet<String> {
        use clap::Parser;

        let mut failures = std::collections::BTreeSet::new();
        for example in examples {
            let Some(argv) = shlex::split(example) else {
                failures.insert(format!("{example} (unbalanced quoting)"));
                continue;
            };
            if let Err(error) = crate::cli::Cli::try_parse_from(&argv) {
                failures.insert(format!("{example} (does not parse: {})", error.kind()));
                continue;
            }
            match example_path(&argv) {
                Some(path) if normalize_path(&path).is_ok() => {}
                Some(path) => {
                    failures.insert(format!("{example} (PATH {path:?} is not rooted)"));
                }
                None => {
                    failures.insert(format!("{example} (not a mega2 browser invocation)"));
                }
            }
        }
        failures
    }

    /// AC-5: every `mega2 browser` example in the two help banners parses and
    /// names a rooted PATH.
    #[test]
    fn help_example_paths_are_rooted() {
        let mut examples = help_examples(MEGA2_EXAMPLES);
        examples.extend(help_examples(MEGA2_BROWSER_EXAMPLES));
        assert!(!examples.is_empty(), "no examples found");
        assert_eq!(
            unrooted_examples(&examples),
            std::collections::BTreeSet::new()
        );
    }

    /// AC-6: the same for the EN and zh-CN command pages.
    #[test]
    fn doc_example_paths_are_rooted() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut examples = Vec::new();
        for page in ["docs/commands/mega2.md", "docs/commands/zh-CN/mega2.md"] {
            let text = std::fs::read_to_string(root.join(page))
                .unwrap_or_else(|error| panic!("read {page}: {error}"));
            examples.extend(markdown_examples(&text));
        }
        assert!(!examples.is_empty(), "no examples found");
        assert_eq!(
            unrooted_examples(&examples),
            std::collections::BTreeSet::new()
        );
    }

    /// AC-8: the same for the website page named by `LIBRA_SITE_MEGA2_DOC`.
    /// Ignored by default and run explicitly (`--ignored`/`--include-ignored`);
    /// an unset variable or unreadable page fails instead of skipping.
    #[test]
    #[ignore = "set LIBRA_SITE_MEGA2_DOC to the website mega2 page and run explicitly"]
    fn site_example_paths_are_rooted() {
        let page = std::env::var("LIBRA_SITE_MEGA2_DOC")
            .expect("LIBRA_SITE_MEGA2_DOC must name the website mega2 page");
        let text =
            std::fs::read_to_string(&page).unwrap_or_else(|error| panic!("read {page}: {error}"));
        let examples = markdown_examples(&text);
        assert!(!examples.is_empty(), "no examples found in {page}");
        assert_eq!(
            unrooted_examples(&examples),
            std::collections::BTreeSet::new()
        );
    }
}
