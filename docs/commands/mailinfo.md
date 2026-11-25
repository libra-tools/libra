# `libra mailinfo`

Extract commit metadata, the commit-message body, and patch text from one
plain-text email patch. This is a small, scriptable subset of `git mailinfo`
and uses the same parser as `libra am`.

## Synopsis

```text
libra mailinfo <MSG> <PATCH> < mail
```

`MSG` and `PATCH` are output file paths. The mail is always read from standard
input, so this command works outside a Libra repository.

## Behavior

The input is limited to 64 MiB and must be UTF-8, single-part `text/plain`.
Supported transfer encodings are `7bit`, `8bit`, `binary`,
`quoted-printable`, and `base64`. `From:`, `Date:`, and `Subject:` are
required. Folded headers, UTF-8/US-ASCII RFC 2047 B/Q encoded words, an
optional mbox `From ` envelope line, a leading `[PATCH ...]` subject marker,
and the standard in-body `From:` override are handled.

On success:

- stdout contains `Author:`, `Email:`, cleaned `Subject:`, and `Date:` lines;
- `MSG` contains only the decoded message body before the `---` separator,
  with a final newline when the body is non-empty;
- `PATCH` contains the `---` separator, diffstat, `diff --git` patch, and any
  trailing signature block.

Both destinations must have existing parent directories, must be distinct
after resolving parent-directory aliases, and cannot be `-` or directories.
The complete input and both temporary payloads are validated/written before
either destination is replaced. Each final file replacement is atomic, though
two separate filesystem paths cannot form one cross-file atomic transaction.

## Output control

| Option | Meaning |
|---|---|
| `--quiet` | Write `MSG` and `PATCH` without metadata on stdout. |
| `--json` / `--machine` | Emit the parsed metadata, output paths, and byte counts in the standard JSON envelope. Files are still written. |

JSON `data` contains `author`, `email`, `subject`, `date`, `message_path`,
`patch_path`, `message_bytes`, and `patch_bytes`.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | The mail was parsed and both output files were replaced. |
| `128` | Input, encoding, header, patch split, path, or file I/O validation failed. |
| `129` | Required output arguments are missing or invalid. |

## Examples

```bash
libra mailinfo message.txt patch.diff < 0001-fix.patch
libra --quiet mailinfo message.txt patch.diff < 0001-fix.patch
libra --json mailinfo message.txt patch.diff < 0001-fix.patch
```

## Current limitations

The minimal P2-02 surface exposes no Git `mailinfo` options such as `-k`,
`-b`, `-m`, `-u`, `--encoding`, `--scissors`, or `--quoted-cr`. MIME
multipart messages, attachments, charsets other than UTF-8/US-ASCII/ISO-8859-1, multi-message mboxes,
binary patches, and patch text without a `diff --git` section are rejected.
