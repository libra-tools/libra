//! Extract commit metadata, message text, and patch text from one mail.
//!
//! The parser in this module is shared with `am` so subject cleanup, transfer
//! decoding, and message/patch splitting have one implementation.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use base64::Engine as _;
use chrono::DateTime;
use clap::Parser;
use serde::Serialize;

use crate::{
    command::apply::MAX_PATCH_BYTES,
    utils::{
        atomic_stream::StreamingAtomicFile,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data, stdout_write_error},
    },
};

pub const MAILINFO_EXAMPLES: &str = "\
EXAMPLES:
    libra mailinfo message patch < 0001-fix.patch
    libra --quiet mailinfo message patch < mail
    libra --json mailinfo message patch < mail";

#[derive(Parser, Debug)]
#[command(after_help = MAILINFO_EXAMPLES)]
pub struct MailinfoArgs {
    /// File that receives the decoded commit-message body.
    #[arg(value_name = "MSG")]
    pub message: PathBuf,

    /// File that receives the extracted patch, beginning at the `---` separator.
    #[arg(value_name = "PATCH")]
    pub patch: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedMail {
    pub(crate) author_name: String,
    pub(crate) author_email: String,
    pub(crate) date: String,
    pub(crate) author_date: String,
    pub(crate) subject: String,
    pub(crate) body_message: String,
    pub(crate) mailinfo_patch: String,
    pub(crate) apply_patch: String,
}

impl ParsedMail {
    pub(crate) fn author(&self) -> String {
        format!("{} <{}>", self.author_name, self.author_email)
    }

    pub(crate) fn commit_message(&self) -> String {
        if self.body_message.is_empty() {
            self.subject.clone()
        } else {
            format!("{}\n\n{}", self.subject, self.body_message)
        }
    }
}

#[derive(Debug, Serialize)]
struct MailinfoOutput<'a> {
    author: &'a str,
    email: &'a str,
    subject: &'a str,
    date: &'a str,
    message_path: String,
    patch_path: String,
    message_bytes: usize,
    patch_bytes: usize,
}

/// Parse one bounded UTF-8 mail from stdin and atomically replace both output
/// files after the complete mail has been validated.
///
/// # Side Effects
/// Replaces `MSG` and `PATCH` independently with same-directory atomic renames.
/// No repository, index, worktree, or object storage is accessed.
///
/// # Errors
/// Rejects oversized/non-UTF-8 input, unsupported mail encodings, malformed
/// metadata, unsafe or aliased output destinations, and output I/O failures.
pub fn execute_safe(args: MailinfoArgs, output: &OutputConfig) -> CliResult<()> {
    let (message_identity, message_parent) = validate_output_path(&args.message, "message")?;
    let (patch_identity, patch_parent) = validate_output_path(&args.patch, "patch")?;
    if message_identity == patch_identity {
        return Err(CliError::command_usage(
            "mailinfo message and patch outputs must be different files",
        ));
    }

    let raw = read_mail_from_stdin()?;
    let parsed = parse_mail("stdin", &raw)?;
    let message = if parsed.body_message.is_empty() {
        Vec::new()
    } else {
        format!("{}\n", parsed.body_message).into_bytes()
    };
    let patch = parsed.mailinfo_patch.as_bytes();

    // Stage both complete payloads before either destination moves, so read,
    // decode, allocation, and temporary-write failures preserve both outputs.
    // Each final replacement is atomic; two distinct filesystem paths cannot
    // be committed as one cross-file transaction.
    let message_staged = stage_output(
        "message",
        &args.message,
        &message_parent,
        message.as_slice(),
    )?;
    let patch_staged = stage_output("patch", &args.patch, &patch_parent, patch)?;
    message_staged
        .persist(&message_identity)
        .map_err(|error| output_write_error("message", &args.message, &message_parent, error))?;
    patch_staged
        .persist(&patch_identity)
        .map_err(|error| output_write_error("patch", &args.patch, &patch_parent, error))?;

    let result = MailinfoOutput {
        author: &parsed.author_name,
        email: &parsed.author_email,
        subject: &parsed.subject,
        date: &parsed.date,
        message_path: args.message.display().to_string(),
        patch_path: args.patch.display().to_string(),
        message_bytes: message.len(),
        patch_bytes: patch.len(),
    };
    if output.is_json() {
        return emit_json_data("mailinfo", &result, output);
    }
    if output.quiet {
        return Ok(());
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Author: {}", result.author)
        .and_then(|()| writeln!(writer, "Email: {}", result.email))
        .and_then(|()| writeln!(writer, "Subject: {}", result.subject))
        .and_then(|()| writeln!(writer, "Date: {}", result.date))
        .map_err(|error| stdout_write_error("write mailinfo metadata", error))
}

fn read_mail_from_stdin() -> CliResult<String> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_PATCH_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            CliError::fatal(format!("failed to read mail from stdin: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    if bytes.len() > MAX_PATCH_BYTES {
        return Err(CliError::fatal(format!(
            "mail input exceeds the {} MiB limit",
            MAX_PATCH_BYTES / (1024 * 1024)
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    String::from_utf8(bytes).map_err(|_| {
        CliError::fatal("mail input from stdin is not valid UTF-8")
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    })
}

fn validate_output_path(path: &Path, label: &str) -> CliResult<(PathBuf, PathBuf)> {
    if path == Path::new("-") {
        return Err(CliError::command_usage(format!(
            "mailinfo {label} output cannot be '-'; provide a file path"
        )));
    }
    let file_name = path.file_name().ok_or_else(|| {
        CliError::command_usage(format!(
            "mailinfo {label} output '{}' has no file name",
            path.display()
        ))
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent).map_err(|error| {
        CliError::fatal(format!(
            "cannot access parent directory '{}' for mailinfo {label} output '{}': {error}",
            parent.display(),
            path.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
        .with_hint("create the parent directory and make it writable, then retry")
    })?;
    if !canonical_parent.is_dir() {
        return Err(CliError::fatal(format!(
            "parent '{}' for mailinfo {label} output '{}' is not a directory",
            parent.display(),
            path.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(CliError::fatal(format!(
                "mailinfo {label} output '{}' is a directory",
                path.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CliError::fatal(format!(
                "cannot inspect mailinfo {label} output '{}': {error}",
                path.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed));
        }
    }
    Ok((canonical_parent.join(file_name), canonical_parent))
}

fn stage_output(
    label: &str,
    path: &Path,
    parent: &Path,
    bytes: &[u8],
) -> CliResult<StreamingAtomicFile> {
    let mut staged = StreamingAtomicFile::new_in(parent, false)
        .map_err(|error| output_write_error(label, path, parent, error))?;
    staged
        .write_all(bytes)
        .map_err(|error| output_write_error(label, path, parent, error))?;
    Ok(staged)
}

fn output_write_error(label: &str, path: &Path, parent: &Path, error: io::Error) -> CliError {
    CliError::fatal(format!(
        "failed to write mailinfo {label} output '{}': {error}",
        path.display()
    ))
    .with_stable_code(StableErrorCode::IoWriteFailed)
    .with_hint(format!(
        "ensure '{}' is writable and has enough free space",
        parent.display()
    ))
}

pub(crate) fn parse_mail(source: &str, raw: &str) -> CliResult<ParsedMail> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let without_envelope = normalized
        .strip_prefix("From ")
        .and_then(|text| text.split_once('\n').map(|(_, rest)| rest))
        .unwrap_or(&normalized);
    let (raw_headers, encoded_body) = without_envelope
        .split_once("\n\n")
        .ok_or_else(|| invalid_mail(source, "missing blank line after mail headers"))?;
    let headers = parse_headers(raw_headers).map_err(|detail| invalid_mail(source, &detail))?;
    let content_type = header(&headers, "content-type").unwrap_or("text/plain");
    let transfer = header(&headers, "content-transfer-encoding").unwrap_or("8bit");
    let body = extract_text_body(content_type, transfer, encoded_body, 0)
        .map_err(|detail| invalid_mail(source, &detail))?;
    if body.contains('\0') {
        return Err(invalid_mail(source, "decoded mail body contains NUL"));
    }

    let author = decode_encoded_words(required_header(&headers, "from", source)?)
        .map_err(|detail| invalid_mail(source, &detail))?;
    let date = required_header(&headers, "date", source)?
        .trim()
        .to_string();
    validate_decoded_header("Date", &date).map_err(|detail| invalid_mail(source, &detail))?;
    let author_date =
        normalize_author_date(&date).map_err(|detail| invalid_mail(source, &detail))?;
    let subject = decode_encoded_words(required_header(&headers, "subject", source)?)
        .map_err(|detail| invalid_mail(source, &detail))?;
    validate_decoded_header("Subject", &subject).map_err(|detail| invalid_mail(source, &detail))?;
    let subject = clean_patch_subject(&subject);
    if subject.is_empty() {
        return Err(invalid_mail(source, "patch subject is empty"));
    }

    let lines: Vec<&str> = body.lines().collect();
    let diff_start = lines
        .iter()
        .position(|line| line.starts_with("diff --git "))
        .ok_or_else(|| invalid_mail(source, "mail body contains no 'diff --git' patch"))?;
    let separator = lines[..diff_start]
        .iter()
        .rposition(|line| *line == "---")
        .ok_or_else(|| invalid_mail(source, "mail patch is missing the '---' separator"))?;
    let mut message_start = lines[..separator]
        .iter()
        .position(|line| !line.is_empty())
        .unwrap_or(separator);
    let author = if let Some(in_body_from) = lines
        .get(message_start)
        .and_then(|line| line.strip_prefix("From: "))
    {
        let decoded = decode_encoded_words(in_body_from.trim())
            .map_err(|detail| invalid_mail(source, &detail))?;
        message_start += 1;
        if lines.get(message_start).is_some_and(|line| line.is_empty()) {
            message_start += 1;
        }
        decoded
    } else {
        author
    };
    let (author_name, author_email) =
        split_author(&author).map_err(|detail| invalid_mail(source, &detail))?;
    let body_message = lines[message_start..separator]
        .join("\n")
        .trim()
        .to_string();

    let mailinfo_patch = format!("{}\n", lines[separator..].join("\n"));
    let mut apply_patch = format!("{}\n", lines[diff_start..].join("\n"));
    if let Some(signature) = apply_patch.find("\n-- \n") {
        apply_patch.truncate(signature + 1);
    }
    Ok(ParsedMail {
        author_name,
        author_email,
        date,
        author_date,
        subject,
        body_message,
        mailinfo_patch,
        apply_patch,
    })
}

fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, String> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in raw.lines() {
        if line.starts_with([' ', '\t']) {
            let (_, value) = headers
                .last_mut()
                .ok_or_else(|| "mail header continuation has no preceding header".to_string())?;
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("malformed mail header '{line}'"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!("invalid mail header name '{name}'"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(headers)
}

/// Cap on multipart nesting (PD-09 ④): real mails nest one or two
/// levels; a hostile boundary bomb must not recurse unbounded.
const MAX_MULTIPART_DEPTH: usize = 4;

/// PD-09 ④: produce the mail's effective TEXT body. `text/plain`
/// decodes directly (the historical behavior, byte-identical);
/// `multipart/*` containers split on their boundary and concatenate
/// every supported text part in order — `text/plain`, `text/x-patch`,
/// and `text/x-diff` (the attachment types `format-patch --attach`
/// produces) — recursing into nested multiparts (bounded) and skipping
/// non-text parts such as HTML alternatives or binary attachments.
fn extract_text_body(
    content_type: &str,
    transfer: &str,
    encoded: &str,
    depth: usize,
) -> Result<String, String> {
    if depth > MAX_MULTIPART_DEPTH {
        return Err("multipart mail nests too deep".to_string());
    }
    let media = media_type_of(content_type);
    if !media.starts_with("multipart/") {
        validate_content_type(content_type)?;
        return decode_transfer(encoded, transfer);
    }
    // RFC 2045: a multipart container itself must use an identity
    // transfer encoding.
    match transfer.trim().to_ascii_lowercase().as_str() {
        "" | "7bit" | "8bit" | "binary" => {}
        other => {
            return Err(format!(
                "unsupported Content-Transfer-Encoding '{other}' on a multipart container"
            ));
        }
    }
    let boundary = content_type_parameter(content_type, "boundary")
        .ok_or_else(|| "multipart mail is missing its boundary parameter".to_string())?;
    if boundary.is_empty() {
        return Err("multipart mail has an empty boundary parameter".to_string());
    }
    let mut collected = String::new();
    for part in split_multipart(encoded, &boundary)? {
        // RFC 2046: a part may have an EMPTY header section — its content
        // starts right after a leading blank line. Detect that before the
        // generic headers/body split so a body paragraph break is never
        // mis-read as the header separator.
        let (raw_part_headers, part_body) = if let Some(body) = part.strip_prefix('\n') {
            ("", body)
        } else if let Some(body) = part.strip_prefix("\r\n") {
            ("", body)
        } else {
            match part.split_once("\n\n") {
                Some((headers, body)) => (headers, body),
                // A headerless part is implicit text/plain.
                None => ("", part.as_str()),
            }
        };
        let part_headers = parse_headers(raw_part_headers)?;
        let part_type = header(&part_headers, "content-type").unwrap_or("text/plain");
        let part_transfer = header(&part_headers, "content-transfer-encoding").unwrap_or("8bit");
        let part_media = media_type_of(part_type);
        if part_media.starts_with("multipart/") {
            let nested = extract_text_body(part_type, part_transfer, part_body, depth + 1)?;
            push_text_part(&mut collected, &nested);
            continue;
        }
        if !matches!(
            part_media.as_str(),
            "text/plain" | "text/x-patch" | "text/x-diff"
        ) {
            // HTML alternatives, binary attachments, signatures … are not
            // patch carriers; skipping them mirrors `git mailinfo`.
            continue;
        }
        validate_text_charset(part_type)?;
        let decoded = decode_transfer(part_body, part_transfer)?;
        push_text_part(&mut collected, &decoded);
    }
    if collected.trim().is_empty() {
        return Err("multipart mail carries no supported text part".to_string());
    }
    Ok(collected)
}

/// Join text parts with a separating newline so a body part that does
/// not end in one cannot glue onto the next part's first line.
fn push_text_part(collected: &mut String, part: &str) {
    if !collected.is_empty() && !collected.ends_with('\n') {
        collected.push('\n');
    }
    collected.push_str(part);
}

/// Split a Content-Type header's `;`-separated parameters WITHOUT
/// breaking inside quoted values (`name="a;b"`), per RFC 2045 §5.1.
fn split_ct_params(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b';' if !in_quotes => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

fn media_type_of(content_type: &str) -> String {
    split_ct_params(content_type)
        .first()
        .copied()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Extract a `name=value` parameter from a Content-Type header,
/// stripping optional quotes.
fn content_type_parameter(value: &str, name: &str) -> Option<String> {
    for parameter in split_ct_params(value).into_iter().skip(1) {
        let Some((candidate, parameter_value)) = parameter.trim().split_once('=') else {
            continue;
        };
        if candidate.trim().eq_ignore_ascii_case(name) {
            return Some(parameter_value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Split a multipart body on its boundary lines. The preamble (before
/// the first boundary) is discarded; the closing `--boundary--` ends the
/// scan; an unterminated final part is tolerated (matching mail
/// tooling). Each part's trailing delimiter newline is removed.
fn split_multipart(body: &str, boundary: &str) -> Result<Vec<String>, String> {
    let open = format!("--{boundary}");
    let close = format!("--{boundary}--");
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in body.split_inclusive('\n') {
        // RFC 2046 §5.1.1: a boundary delimiter line may carry trailing
        // transport padding (spaces/tabs) before the CRLF.
        let bare = line
            .trim_end_matches(['\n', '\r'])
            .trim_end_matches([' ', '\t']);
        if bare == close {
            if let Some(part) = current.take() {
                parts.push(trim_part_delimiter_newline(part));
            }
            break;
        }
        if bare == open {
            if let Some(part) = current.take() {
                parts.push(trim_part_delimiter_newline(part));
            }
            current = Some(String::new());
            continue;
        }
        if let Some(part) = current.as_mut() {
            part.push_str(line);
        }
    }
    if let Some(part) = current.take() {
        parts.push(trim_part_delimiter_newline(part));
    }
    if parts.is_empty() {
        return Err("multipart mail has no parts for its declared boundary".to_string());
    }
    Ok(parts)
}

/// The newline immediately before a boundary belongs to the delimiter,
/// not to the part content (RFC 2046 §5.1.1).
fn trim_part_delimiter_newline(mut part: String) -> String {
    if part.ends_with('\n') {
        part.pop();
        if part.ends_with('\r') {
            part.pop();
        }
    }
    part
}

fn validate_content_type(value: &str) -> Result<(), String> {
    let binding = media_type_of(value);
    let media_type = binding.as_str();
    if !media_type.eq_ignore_ascii_case("text/plain") {
        return Err(format!(
            "unsupported Content-Type '{media_type}'; expected text/plain or a multipart container"
        ));
    }
    validate_text_charset(value)
}

fn validate_text_charset(value: &str) -> Result<(), String> {
    for parameter in split_ct_params(value).into_iter().skip(1) {
        let Some((name, parameter_value)) = parameter.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("charset") {
            let charset = parameter_value.trim().trim_matches('"');
            if !matches!(charset.to_ascii_lowercase().as_str(), "utf-8" | "us-ascii") {
                return Err(format!("unsupported text charset '{charset}'"));
            }
        }
    }
    Ok(())
}

fn validate_decoded_header(name: &str, value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!(
            "decoded {name} header contains a control character"
        ));
    }
    Ok(())
}

fn split_author(author: &str) -> Result<(String, String), String> {
    validate_decoded_header("From", author)?;
    let author = author.trim();
    let Some(start) = author.find('<') else {
        return Err("From header must use 'Name <email>' format".to_string());
    };
    let Some(relative_end) = author[start..].find('>') else {
        return Err("From header must use 'Name <email>' format".to_string());
    };
    let end = start + relative_end;
    let name = author[..start].trim();
    let email = author[start + 1..end].trim();
    if name.is_empty()
        || email.is_empty()
        || end != author.len() - 1
        || name.contains(['<', '>'])
        || email.contains(['<', '>'])
    {
        return Err("From header must use 'Name <email>' format".to_string());
    }
    Ok((name.to_string(), email.to_string()))
}

pub(crate) fn validate_author(author: &str) -> Result<(), String> {
    split_author(author).map(|_| ())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.as_str())
}

fn required_header<'a>(
    headers: &'a [(String, String)],
    name: &str,
    source: &str,
) -> CliResult<&'a str> {
    header(headers, name)
        .ok_or_else(|| invalid_mail(source, &format!("missing required {name} header")))
}

fn decode_transfer(body: &str, encoding: &str) -> Result<String, String> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "7bit" | "8bit" | "binary" => Ok(body.to_string()),
        "base64" => {
            let compact: String = body
                .chars()
                .filter(|ch| !ch.is_ascii_whitespace())
                .collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(compact)
                .map_err(|error| format!("invalid base64 mail body: {error}"))?;
            String::from_utf8(bytes).map_err(|_| "decoded mail body is not UTF-8".to_string())
        }
        "quoted-printable" => decode_quoted_printable(body),
        other => Err(format!("unsupported Content-Transfer-Encoding '{other}'")),
    }
}

fn decode_quoted_printable(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'=' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'\n') {
            index += 2;
            continue;
        }
        let high = bytes
            .get(index + 1)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| "invalid quoted-printable escape".to_string())?;
        let low = bytes
            .get(index + 2)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| "invalid quoted-printable escape".to_string())?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| "decoded mail body is not UTF-8".to_string())
}

fn decode_encoded_words(input: &str) -> Result<String, String> {
    let mut output = String::new();
    let mut rest = input;
    let mut previous_was_encoded = false;
    while let Some(start) = rest.find("=?") {
        let prefix = &rest[..start];
        if !previous_was_encoded || !prefix.chars().all(char::is_whitespace) {
            output.push_str(prefix);
        }
        let word = &rest[start + 2..];
        let (charset, after_charset) = word
            .split_once('?')
            .ok_or_else(|| "malformed RFC 2047 encoded word".to_string())?;
        let (encoding, after_encoding) = after_charset
            .split_once('?')
            .ok_or_else(|| "malformed RFC 2047 encoded word".to_string())?;
        let (encoded, after_word) = after_encoding
            .split_once("?=")
            .ok_or_else(|| "malformed RFC 2047 encoded word".to_string())?;
        if !matches!(charset.to_ascii_lowercase().as_str(), "utf-8" | "us-ascii") {
            return Err(format!("unsupported RFC 2047 charset '{charset}'"));
        }
        let decoded = match encoding.to_ascii_lowercase().as_str() {
            "b" => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|error| format!("invalid RFC 2047 base64 word: {error}"))?;
                String::from_utf8(bytes)
                    .map_err(|_| "decoded RFC 2047 word is not UTF-8".to_string())?
            }
            "q" => decode_quoted_printable(&encoded.replace('_', " "))?,
            other => return Err(format!("unsupported RFC 2047 encoding '{other}'")),
        };
        output.push_str(&decoded);
        rest = after_word;
        previous_was_encoded = true;
    }
    output.push_str(rest);
    Ok(output)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn clean_patch_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if let Some(close) = trimmed.find(']')
        && trimmed.starts_with('[')
        && matches!(
            trimmed[1..close]
                .trim()
                .split_ascii_whitespace()
                .next(),
            Some(marker) if marker.eq_ignore_ascii_case("patch")
        )
    {
        return trimmed[close + 1..].trim().to_string();
    }
    trimmed.to_string()
}

fn normalize_author_date(value: &str) -> Result<String, String> {
    let date = DateTime::parse_from_rfc2822(value)
        .map_err(|error| format!("invalid Date header '{value}': {error}"))?;
    let seconds = date.offset().local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let absolute = seconds.unsigned_abs();
    Ok(format!(
        "{} {sign}{:02}{:02}",
        date.timestamp(),
        absolute / 3600,
        (absolute % 3600) / 60
    ))
}

pub(crate) fn invalid_mail(source: &str, detail: &str) -> CliError {
    CliError::fatal(format!("invalid mail patch '{source}': {detail}"))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIL: &str = "From 0123456789 Mon Sep 17 00:00:00 2001\n\
From: Alice Example <alice@example.com>\n\
Date: Tue, 14 Jul 2026 10:00:00 +0800\n\
Subject: [PATCH 1/1] fix greeting\n\
Content-Type: text/plain; charset=UTF-8\n\
Content-Transfer-Encoding: 8bit\n\
\n\
Explain why.\n\
---\n\
\x20file.txt | 2 +-\n\
\x201 file changed, 1 insertion(+), 1 deletion(-)\n\
\n\
diff --git a/file.txt b/file.txt\n\
--- a/file.txt\n\
+++ b/file.txt\n\
@@ -1 +1 @@\n\
-old\n\
+new\n\
-- \n\
libra 0.18.84\n";

    #[test]
    fn parses_plain_format_patch_mail_for_mailinfo_and_am() {
        let parsed = parse_mail("one.patch", MAIL).expect("parse mail");
        assert_eq!(parsed.author(), "Alice Example <alice@example.com>");
        assert_eq!(parsed.subject, "fix greeting");
        assert_eq!(parsed.body_message, "Explain why.");
        assert_eq!(parsed.commit_message(), "fix greeting\n\nExplain why.");
        assert!(parsed.mailinfo_patch.starts_with("---\n file.txt"));
        assert!(parsed.mailinfo_patch.contains("libra 0.18.84"));
        assert!(!parsed.apply_patch.contains("libra 0.18.84"));
    }

    #[test]
    fn decodes_quoted_printable_and_encoded_subject() {
        assert_eq!(
            decode_quoted_printable("hello=20world=0A").expect("decode"),
            "hello world\n"
        );
        assert_eq!(
            decode_encoded_words("=?UTF-8?Q?fix=3A_caf=C3=A9?=").expect("decode"),
            "fix: café"
        );
        assert_eq!(
            decode_encoded_words("=?UTF-8?Q?fix=3A?= =?UTF-8?Q?_caf=C3=A9?=")
                .expect("decode adjacent words"),
            "fix: café"
        );
    }

    #[test]
    fn cleans_only_patch_subject_prefix() {
        assert_eq!(clean_patch_subject("[PATCH v2 2/3] topic"), "topic");
        assert_eq!(clean_patch_subject("[RFC] topic"), "[RFC] topic");
        assert_eq!(clean_patch_subject("[dispatch] topic"), "[dispatch] topic");
    }

    #[test]
    fn rejects_unsupported_content_types_and_header_injection() {
        assert!(validate_content_type("text/plain; charset=UTF-8").is_ok());
        assert!(validate_content_type("multipart/mixed; boundary=x").is_err());
        assert!(validate_content_type("text/plain; charset=iso-8859-1").is_err());

        let decoded = decode_encoded_words("=?UTF-8?B?QWxpY2UK?= <alice@example.com>")
            .expect("decode injected header");
        assert!(split_author(&decoded).is_err());
        assert!(split_author("Alice Example <alice@example.com>").is_ok());
        assert!(split_author("Alice <alias <alice@example.com>").is_err());
    }
}
