# `libra media`

FastCDC LFS media chunking client (lore.md §6) — a **feature-gated** Libra
extension (`fastcdc`, compiled only into builds with `--features fastcdc`;
**absent from the default binary**). It content-defines chunks of a media file,
builds a versioned manifest, stores chunks in a private local store, reassembles
and verifies them, and negotiates a remote's chunked-LFS capability with a safe
fallback to standard Git LFS.

`media` is a Libra-only extension (`intentionally-different`): Git has no media
chunking concept. The Git object graph is never touched — a chunk is never a Git
object ID, and chunks/manifests live in a private `.libra/media/` store that is a
sibling of `objects/`. The `media_oid` is always SHA-256 of the full file
(independent of `core.objectformat`), byte-identical to a standard LFS pointer
OID.

## Subcommands

| Subcommand | Description | Example |
|---|---|---|
| `chunk <path> [--store]` | FastCDC-chunk a file and emit its manifest; `--store` persists chunks + manifest to `.libra/media`. | `libra media chunk big.psd --store` |
| `inspect <manifest>` | Parse and validate a manifest JSON file. | `libra media inspect .libra/media/manifests/<oid>.json` |
| `verify <path> \| --media-oid <oid>` | Reassemble from the local chunk store and verify the full `media_oid` (never publishes a corrupt file). | `libra media verify big.psd` |
| `probe [--remote <name>]` | Probe the remote's media capability endpoint and report the transfer decision (chunked vs standard-LFS fallback). | `libra media probe --remote origin` |
| `--json` | Structured JSON envelope on stdout (global flag). | `libra --json media chunk big.psd` |

## Safe fallback

`media probe` reports the remote's capabilities: `chunked (fastcdc-v1)` or
`standard-lfs (fallback)` with a reason such as no capability endpoint, disabled
server support, incompatible algorithm, insufficient required capabilities,
unknown protocol version, or a server error after backoff. It assumes the
repository permits chunking and a complete local fallback object is available;
it does **not** read `lfs.fastcdc` and does not report `blocked` under these
assumptions. A `chunked` probe result therefore does not prove that transfers are
enabled in this repository.

Actual LFS transfers also apply `lfs.fastcdc` and require the server to retain a
complete standard-LFS fallback and accept manifests. Chunk-only advertisements
use basic LFS instead. Mega built with `--features fastcdc` implements the
authenticated extension; other remotes retain the standard Git LFS fallback.

## Live LFS transfers with Mega

Build Libra with `cargo build --features fastcdc` and, in the Mega repository,
build/start the HTTP server with
`cargo run -p mono --features fastcdc -- service http` using its normal server
configuration. Both builds default to feature OFF. The `libra` commands below
must use the feature-built binary (`target/debug/libra`, or `libra.exe` on
Windows); compiling does not replace a separately installed binary on PATH.

Obtain a **Mono-issued access token** through Mega's existing authenticated
token-creation flow (`POST /api/v1/user/token/generate`). `libra auth login`
only stores that token locally; it does not issue a Mega token. A GitHub PAT or
browser session cookie is not a substitute for the Mono access token.

For a local Mega HTTP server on port 8000, run in the Libra repository:

```bash
libra config remote.origin.url http://localhost:8000/project/demo.git
libra auth login --host http://localhost:8000
# Paste the Mono access token at the hidden prompt.
libra auth status --host http://localhost:8000
libra config lfs.fastcdc true
libra media probe --remote origin
```

After compiling the feature, an unset `lfs.fastcdc` permits automatic negotiation;
`true` explicitly enables it and `false` disables it in that repository. The
stored token must match the remote's **host and port**. Use HTTPS for non-loopback
servers (for example `--host https://mega.example.com:8443`); HTTP token attachment
is allowed only for loopback. Pass only the origin to `--host`, without the
repository path, and do not put tokens in URLs. For scripts, feed the token on
stdin with `--with-token`; see [`libra auth`](auth.md).

Keep the repository URL in `origin`. The LFS client preserves
`<repo>.git/info/lfs`; capability discovery appends `libra/media/v1/capabilities`
to that LFS URL. The Bearer header is attached automatically from the stored token.

Normal LFS push/upload now prepares a versioned manifest, uploads only missing
chunks, and finalizes. Mega verifies chunk hashes, full SHA-256 and the frozen
FastCDC boundaries, writes the complete standard-LFS object, then publishes the
manifest. Repeating push resumes from missing chunks. Downloads use finalized
manifests, reuse verified local chunks, and atomically publish only verified full
content. Invalid manifests or corrupted remote chunks are errors and preserve the
existing destination. No manifest, unsupported capabilities or disabled feature
means standard full-object LFS. Objects exceeding the negotiated manifest/chunk
count limits also use basic upload before any manifest is sent. Chunk-only uploads
are not supported. Outside a Libra repository, the public LFS download client uses
basic LFS instead of creating a repository cache.

Mega's initial extension isolates chunks by authenticated user and repository;
another user's data is fetched through the standard full-object fallback. It
requires Bearer access tokens and does not introduce a public chunk-hash API.
Manifests are limited to 10 MiB / 8192 chunks and chunks to 8 MiB. This is an
opt-in transport; deployments need explicit retention and quota planning.

## Deferred

Shared repository ACLs, automatic orphan GC, quota accounting, server fsck/heal,
obliteration, chunk-only policy and byte-range hydration remain deferred. The
current implementation does not claim completion of all Lore §6.5–6.8 guarantees.

## Examples

```bash
libra media chunk big.psd                 # chunk a file; print the manifest summary
libra media chunk big.psd --store         # also persist chunks + manifest locally
libra media inspect .libra/media/manifests/<oid>.json
libra media verify big.psd                # reassemble from the store and verify media_oid
libra media probe --remote origin         # capability-probe; falls back to standard LFS
libra --json media chunk big.psd          # structured JSON output for agents
```
