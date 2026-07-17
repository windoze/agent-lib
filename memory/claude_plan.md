# M9-5 更新 docs/examples/capability matrix

**当前任务 = TODO.md 首个未完成 = M9-5**（`### [TODO] M9-5`, line 2939）。M1..M9-4 全 `[DONE]`。
下一个 `[TODO]` 是 M9-6（总体 Review），不属于本次。

## 任务要求（TODO.md 2939-2972）
更新文档 + 新增/更新 examples，让文档反映 **实测/as-built** 状态而非目标状态。

- 更新 docs：
  - `docs/managed-external-agent.md`（§3 能力 parity 表仍写「runtime handler 待实现」——需同步为已落地）。
  - `docs/capability-matrix.md`（Managed External Runtime 能力模型一节需反映 M5–M9 实际状态）。
  - `AGENTS.md`（不存在，任务说「如需要」→ 新建，含 managed external 运行说明）。
- 新增/更新 examples：Claude Code managed / Codex managed / OpenCode managed / mixed external agents。
  - 必须展示 **scoped effect wiring**（machine + scope + external handler），不能直接调 adapter 绕过 machine。
- 文档必须说明：feature flags、required env vars、ignored test 命令、worktree isolation、
  secret redaction、unsupported capability fallback。

## 验证条件（TODO.md 2966-2972）
- `cargo test --all --all-targets`（默认）。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- examples 可编译：`cargo check --examples`（默认）+ `cargo check --examples --features "..."`（feature-gated）。
- `git diff --check`。
- 完成记录列出文档和 example 路径。

## 已核对的 API（examples 用）
- 三个 adapter 接口一致：`XxxConfig::new().with_working_dir().with_permission_mode(ExternalPermissionMode)
  .with_timeout(Duration).with_binary(String)[.with_model()]` → `probe(&config).await -> ExternalRuntimeCapabilities`
  → `XxxAdapter::with_probed_capabilities(config, &probed)` → `ExternalSessionRegistry::new(Arc::new(adapter))`。
  - claude: `probe`；codex: `codex_probe`；opencode: `opencode_probe`（`agent_lib::agent::external`，feature-gated）。
- registry-backed handler（模仿 e2e `ManagedRuntimeHandler`）：`registry.get_or_start(request, ctx, Some(sink)).await`
  → `handle.lock().await.advance(&request.input, ctx).await` → `RuntimeDecisionPoint`（`.into()` → `ExternalSessionResult`）。
  `impl ExternalSessionHandler { async fn fulfill -> RequirementResult::ExternalSession(Box::new(...)) }`。
- machine：`ExternalAgentSpec::new(agent_id, runtime, WorktreeRef, None, ToolSetRef, ExternalSessionPolicy)`
  → `ExternalAgentState::new(spec, Conversation)` → `ExternalAgentMachine::new(state, Arc<dyn RequirementIds>)`。
- 驱动：testkit `drain(&mut machine, user_input(ids, prompt), &scope, None, &ctx).await`。
  scope = `TestScope::builder().external(handler).interaction(ScriptedInteractionHandler::approve_all()).build()`。
  ids = testkit `SeqIds`（impl `RequirementIds`），`root_context`/`user_input` = testkit fixtures。
- sink：`impl ExternalEventSink { fn emit(&self, &ExternalObservedEvent) }`（`event.event` = `ExternalAgentEvent`）。
- cleanup：`registry.cleanup_agent(agent_id).await`；worktree = 临时 `git init` 目录，结束 remove。
- `ExternalRuntimeKind::{ClaudeCode, Codex, OpenCode, Custom(String)}`。

## examples 落地方案
- 共享 helper：`examples/support/managed.rs`（**不**被 `support/mod.rs` 引用；各 managed example 用
  `#[path = "support/managed.rs"] mod managed;` 直接 include）。含 `CountingSink`、`ObservationLog`、
  `RegistryHandler`（runtime-agnostic）、cfg-gated `build_registry` per runtime、
  `drive_managed_child(...) -> Outcome/skip`、worktree/env/secret-safe helpers。使用 dev-dep `agent_testkit`。
- 4 个 example（`Cargo.toml` `[[example]]` + `required-features` 门控，默认 `cargo check --examples` 跳过）：
  managed_claude_code / managed_codex / managed_opencode / managed_mixed。
- 每个 example：CLI 缺失/probe 失败 → 打印非密 skip 并 exit 0。

## docs 落地方案
- `managed-external-agent.md` §3 parity 表翻新 + §21 M9 里程碑标 as-built + 指向 examples。
- `capability-matrix.md` Managed 节补 Codex/OpenCode adapter 落地 + examples 指针 + 实测仍以 ignored e2e 为准。
- 新建 `AGENTS.md`：仓库总览 + managed external 运行说明。

## 进度
- [ ] 写 examples/support/managed.rs
- [ ] 4 examples + Cargo.toml [[example]]
- [ ] docs: managed-external-agent.md §3/§21
- [ ] docs: capability-matrix.md managed 节
- [ ] AGENTS.md
- [ ] 验证 + 标 [DONE] + commit

## 完成状态（M9-5 DONE）
- [x] examples/support/managed.rs（shared registry-backed handler + drive）
- [x] 4 examples + Cargo.toml [[example]] required-features 门控
- [x] docs/managed-external-agent.md §3 parity 翻新 + §21 M9 里程碑
- [x] docs/capability-matrix.md 新增「可运行示例与真机验证入口（M9-5）」
- [x] 新增 AGENTS.md + README.md 示例/参考链接
- [x] 验证全过：fmt/clippy(default+全feature+单feature)/check-examples(default+feature)/
      test-all(default 47 ok)/test-all(feature 51 ok)/doc(-D warnings)/git diff --check
- [x] TODO.md M9-5 标 [DONE] + 完成记录
- 下一个未完成 = M9-6（总体 Review），留给下次 invocation。
