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

## Malformed HTTP(S) discovery responses

During HTTP(S) reference discovery, Libra rejects a zero-byte advertisement and
malformed pkt-line frames, including short or non-hexadecimal headers, frame
lengths below four, and truncated payloads. A valid `0000` flush remains distinct
from an absent response; a valid empty-repository advertisement is supported.
An unsupported object-format capability reports the fixed message
`Unsupported object format capability` without echoing its remote value.
Check that the URL points to a Git smart HTTP service and that a proxy has not
truncated or replaced the response; then retry.

## pkt-line error classification

Detected pkt-line framing errors return `LBR-NET-002` (exit 128), including an
empty HTTP(S) discovery advertisement. Ordinary connection failures, resets and
timeouts return `LBR-NET-001` (exit 128). Verify the Git service and any proxy
response when a protocol error occurs. Discovery framing errors use the hint
`check that the remote serves Git data and that a proxy has not altered the response`.

This classification applies to reference discovery. SSH authentication and host-trust errors are described below; local
configuration read errors keep their existing error codes.

## SSH advertisement error handling

SSH advertisement lengths `0001` through `0003`, incomplete headers (including
zero-byte EOF), and truncated payloads return `LBR-NET-002`. The fixed protocol
reason and marker are retained without captured SSH stdout/stderr.

An incomplete required header has one host-trust exception: local SSH exit status
255 together with a recognized host-key diagnostic in the first 64 KiB of stderr
returns fixed host-verification guidance and `LBR-NET-001`. This classification
does not verify the remote fingerprint. Other missing advertisements, including
authentication failures, still use `LBR-NET-002`; an available non-zero local exit
status adds `SSH exited with status N` and fixed connectivity, trusted-host,
ssh-agent and repository-access guidance. Original SSH diagnostic text is hidden.

After an incomplete required header, Libra allows up to 100 milliseconds to
observe the SSH exit status, then requests termination if needed. Other read
errors request termination immediately. The status window, direct-child reap and
output collection share a two-second cleanup deadline. Protocol and typed
host-trust errors take precedence over secondary cleanup warnings. Ordinary IO
and timeout errors keep their transport classification and may include a fixed
local cleanup warning. Termination can change the observed exit status. This
does not promise cleanup of arbitrary descendant processes.

Clone places targeted host-verification guidance in its structured hints. The
other command boundaries retain fixed host guidance in the message and their
existing `LBR-NET-001` network hint. Human, JSON and machine diagnostics omit raw
captured remote stderr in either case.

The `git://` object-fetch path already classifies the listed frame errors as
`LBR-NET-002`; Git discovery and non-ASCII/non-hex async header classifications
remain separate work. HTTP(S) framing behavior is unchanged.

## SSH authentication and captured diagnostics

Libra invokes SSH with `BatchMode=yes` for both terminal and non-terminal callers.
It does not prompt for a private-key passphrase or an interactive host-key
decision during a Libra command. Load or unlock an encrypted key in `ssh-agent`
before retrying. For host trust, verify the fingerprint through a trusted
provider console or another trusted channel before manually updating
`~/.ssh/known_hosts`. Alternatively, make a separate interactive SSH connection
and compare the displayed fingerprint before accepting it. For example,
`ssh -T git@github.com` uses GitHub; use the actual repository SSH user, host and
port. Do not accept a fingerprint that has not been verified.

`ssh.strictHostKeyChecking` retains its existing `ask`, `yes`, `accept-new` and
`no` values. `ask` leaves that SSH option to the user's SSH configuration;
`BatchMode=yes` still prevents interactive decisions. Explicit values are
forwarded to SSH. Choose a host-trust policy appropriate to your repository.

SSH stderr is always captured, including in terminal sessions. It is drained
from process startup, retaining at most 64 KiB while counting and hashing the
remaining bytes. User-facing errors contain fixed text and a local exit status
when available. Raw remote stderr is neither printed nor logged. Debug diagnostics
contain only the status, total and retained byte counts, and a SHA-256 digest of
the collected stream. Failed or cancelled collection may prevent these metadata
from being reported; no completed digest is claimed in that case. Hashing work
is proportional to the number of bytes drained.

SSH reference advertisements and receive-pack responses each have a 16 MiB
aggregate limit. An oversized advertisement fails with `LBR-NET-001` and guidance
to use the repository’s HTTPS URL if available, or ask its maintainer to reduce refs. An oversized push response fails with `LBR-NET-001`
and guidance to push fewer refs; it is not accepted as a truncated success.
These limits can affect repositories with very large ref sets or updates. The
streamed fetch pack is not subject to this cap. A failed push response does not
prove that the server rolled back its refs: inspect the remote state before
retrying. Existing IO timeouts still apply.

After a complete discovery advertisement, Libra allows up to 100 milliseconds
for SSH to exit before requesting termination, within a two-second total
cleanup deadline. Captured-output tasks are cancelled when their owner exits or
their deadline expires, including when a descendant keeps a pipe open.

### SSH host identity and diagnostic collection

SSH host identity changes retain a distinct fixed warning: the change may
indicate interception or legitimate key rotation. Verify the new fingerprint
through a trusted channel before replacing an existing known_hosts entry; do not
bypass host-key checking. Unknown and changed host keys both use LBR-NET-001,
but their fixed messages and guidance differ.

A stderr collection timeout does not by itself discard complete protocol output
and an observed local exit status. Non-zero exit status and primary read errors
still fail the operation. Unavailable diagnostics produce only a fixed debug
notice, without fabricated empty-stream counts or digests. Stdout collection or
process-wait failures retain their normal error handling.

### SSH limits and host-classification boundaries

These fixed 16 MiB advertisement and receive-pack response limits apply only to
Libra's SSH transport. The HTTPS and Git transports do not impose this particular
cap. If the server provides an HTTPS endpoint, use its HTTPS remote URL when an
SSH advertisement exceeds the cap; this does not require a read-only user to
change the server's refs. Otherwise, ask the repository maintainer to reduce the
advertised ref set. The streamed fetch pack remains outside this aggregate cap.

Host-trust classification requires an incomplete first header with no stdout
bytes observed, local exit 255 and a recognized retained stderr pattern. Once
any stdout byte arrives, including a partial header, host-like stderr cannot
select host-specific guidance. Failures after a complete advertisement retain
fixed generic diagnostics. The pre-advertisement pattern remains a diagnostic
heuristic, not fingerprint verification.

A successful discovery whose child waits for a request normally incurs the full
100 ms native-exit observation window, once per discovery operation. This is
separate from the two-second direct-child cleanup budget; no benchmark or
arbitrary-descendant cleanup guarantee is implied.
