# `libra ls-remote`

List references advertised by a remote repository without downloading objects or updating local refs.

```bash
libra ls-remote [OPTIONS] <repository> [patterns...]
```

`<repository>` can be a configured remote name when run inside a Libra repository, a URL, or a local Git/Libra repository path.

## Options

| Flag | Description | Example |
|------|-------------|---------|
| `--heads` | Show only `refs/heads/*` branch refs | `libra ls-remote --heads origin` |
| `-t`, `--tags` | Show only `refs/tags/*` tag refs | `libra ls-remote --tags origin` |
| `--refs` | Omit `HEAD` and peeled tag refs ending in `^{}` | `libra ls-remote --refs origin` |
| `--get-url` | Resolve and print the configured URL without contacting the remote | `libra ls-remote --get-url origin` |
| `--exit-code` | Exit with status 2 when discovery succeeds but no refs match | `libra ls-remote --exit-code origin main` |
| `--sort <KEY>` | Sort refs by `refname`, `-refname`, `version:refname`, or `-version:refname` | `libra ls-remote --sort=version:refname --tags origin` |
| `--symref` | Print symbolic-ref targets advertised by the remote (e.g. `ref: refs/heads/main\tHEAD`) above the matching ref | `libra ls-remote --symref origin` |
| `patterns...` | Match full ref names or trailing path components; `*` and `?` follow Git-style glob behavior and can match `/` | `libra ls-remote origin main 'refs/heads/*'` |

## Human Output

Each matching ref is printed as:

```text
<object-id>	<refname>
```

Example:

```text
4f3c2d1a...	HEAD
4f3c2d1a...	refs/heads/main
```

## JSON Output

With `--json`, output uses the standard command envelope:

```json
{
  "ok": true,
  "command": "ls-remote",
  "data": {
    "remote": "origin",
    "url": "https://example.com/repo.git",
    "heads_only": false,
    "tags_only": false,
    "refs_only": false,
    "get_url": false,
    "exit_code": false,
    "sort": null,
    "patterns": [],
    "entries": [
      {
        "hash": "4f3c2d1a...",
        "refname": "refs/heads/main"
      }
    ]
  }
}
```

## Examples

```bash
# List all refs from a named remote
libra ls-remote origin

# List all refs from a URL directly (no remote registration required)
libra ls-remote https://example.com/repo.git

# Restrict to branches matching a pattern
libra ls-remote --heads origin main

# Resolve a configured remote URL without discovery
libra ls-remote --get-url origin

# Sort tags with version-aware refname ordering
libra ls-remote --sort=version:refname --tags origin

# Show symbolic-ref targets (HEAD) advertised by the remote
libra ls-remote --symref origin

# Structured JSON envelope for agents, tags only
libra --json ls-remote --tags origin
```

The same banner is rendered by `libra ls-remote --help` so the doc and
the CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout,
see `docs/development/commands/_general.md` item B).

## Notes

- `ls-remote` performs only protocol discovery (the in-process equivalent of `git-upload-pack --advertise-refs` for local Git repositories — Libra reads their refs directly).
- It does not write objects, remote-tracking refs, config, or working-tree files.
- `--heads` and `--tags` can be combined to show both branch and tag refs while excluding `HEAD`.
- `--get-url` exits before protocol discovery and prints the same redacted URL form used by remote diagnostics.
- `--exit-code` is a silent script signal: no matches returns status 2 without rendering an error.
- `--symref` prints a `ref: <target>\t<name>` line above a symbolic ref's own OID line when its name survives the active filters. Advertised `symref=` capabilities remain authoritative. If a transport omits that capability (notably a local Libra source), Libra derives `HEAD` from the advertised HEAD OID and branch tips using the same deterministic resolver as fetch (OID match, then `main`, `master`, first branch). JSON reports the same result in `symrefs[]`.
