# Session Capture lifecycle decision

AgentTraces ingress lowers every trusted hook to `LifecycleEvent`, then asks
`ai::capture::state::reduce_lifecycle` for the durable `agent_session.state`,
terminal timestamp mutation, revision expectation, and checkpoint action.
The reducer is pure: it accepts the current durable state, event id, injected
time/deadline, and `LifecycleEventKind`; it does not open a repository, read a
transcript, or call `transition_phase`.

缺少 `repo_path` 绝不允许绕过需要 checkpoint 的终态 action：coordinator 会保留
content-free、可重放的 pending-finalize receipt，而不会写入 `stopped` 或伪造
checkpoint。只有后续具备已验证 repository scope 的重放实际得到耐久 checkpoint
并完成 receipt，终态才可发布。Owner filtering、scope binding 和 durable catalog
adapter remain outside the reducer. Import and explicit `libra agent session
resume` retain their entry-specific semantics; consumers must use the complete
terminal predicate (`state='stopped'` plus a non-null `stopped_at`), never the
state string alone.

AgentTraces 的 session_state 与 checkpoint 类别只来自
`capture::state::reduce_lifecycle`。`transition_phase` 仅服务
`HookTarget::AiIntent` 的旧 session metadata 兼容投影；它不是 AgentTraces
的 durable state machine。该 reducer 不读取 `SessionPhase`，也不复制 Entire
的 git session 存储。
