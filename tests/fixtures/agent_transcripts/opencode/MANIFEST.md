# OpenCode Hook/export source contract — synthetic fixture

opencode_pin: 2.0.26
opencode_commit: 9b4ec5714d481559990db0a816d5dec19541a814
standalone_events: session.created, session.inbox.delivered, session.execution.succeeded, session.execution.failed, session.execution.interrupted, session.deleted, session.compaction.ended
plugin_hook_events: tool.execute.after
plugin_merged_events: session.inbox.enqueued
compatibility_events: session.status, message.updated, session.compacted, server.instance.disposed
deprecated_aliases: session.idle
terminal_interruption_reasons: user, superseded, inactivity
status_filtered: busy, retry

`hook_events.json` is hand-authored against the fixed public source. It contains eight minimal Libra observation envelopes: seven standalone events and one tool hook. The enqueue name is a merged prompt source, not an additional turn start. The event vocabulary, compatibility aliases, interruption reasons and filtered statuses bind to OG-00's inline source record; OG-01 owns the actual registry/parser conformance target.

No fixture is a capture of a real session, a real model response, or proof of native reasoning origin. Current source schema uses `{id,type,created,data}`; these envelopes are the minimized Libra ingress form after observation, not complete raw durable bus records. The cwd and identifiers are synthetic.

`session.status` and `session.idle` are schema-defined compatibility inputs, with no execution publisher in this pin. `message.updated`, `session.compacted`, and `server.instance.disposed` are compatibility inputs, not advertised current upstream events. Shutdown interruption preserves restart continuity. Capture is observe-only and does not change provider sessions.

CLI `opencode session export` without `--sanitize` encodes `PublicSessionTransfer={info:PublicSessionInfo,messages:PublicSessionMessage[]}` and filters unsettled messages. PublicInfo keeps SessionInfo fields but replaces location with `{directory}`; location-switched messages replace current and previous locations the same way, excluding their workspaceID. Other message variants retain their schema. App `SessionExportData={info:SessionInfo,messages:SessionMessageInfo[]}` is independently constructed from paginated APIs; the CLI settled filter is not an app-path guarantee. Neither source audit proves live delivery or macOS exporter isolation. The 2.0.26 policy-aware plugin loader may refuse setup even when the file exists. Real end-to-end capture, native Source A, typed sinks and final platform/release gates remain separate pending work.

Owner: OG-00. Current main originally lacked this fixture directory; this card restores these no-code synthetic artifacts. Historical missing baseline is handled by EX-OG-00-BASE current artifact-boundary audit, without reconstructing or claiming a historical diff PASS. Subsequent changes must keep pin/commit and ordered vocabulary equal to the inline source record, and pass current source, privacy and boundary checks.
