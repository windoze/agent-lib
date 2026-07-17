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

## M9-6（总体 Review）执行计划（当前 invocation）
性质：最终验收 review。默认代码应已完整；我的职责是逐项核对 + 跑验证 + 记录，
若发现缺陷则修复或新增任务。

### 步骤
- [ ] 逐项核对 §3 parity 表 13 项（文本/多轮/流式/tool/approval/question/subagent/cancel/budget/artifact/worktree/reconfig/snapshot）都有源码 + 测试支撑
- [ ] 核对 PLAN.md §风险每条有测试或明确限制
- [ ] 确认所有真实 e2e `#[ignore]`；cassette fixtures 脱敏（is_secret_free 等）
- [ ] 确认默认 feature 无重 runtime 依赖（三 adapter feature off-by-default）
- [ ] 确认 ExternalAgentMachine 仍无 IO
- [ ] 验证序列 1-6：fmt --check / clippy(-D warnings, default + 全 feature) / test --all / doc(-D warnings) / git diff --check
- [ ] feature-gated cassette tests：claude/codex/opencode
- [ ] 真机 ignored e2e：有环境则跑并记录；否则记录 skip 条件
- [ ] 完成记录写最终能力矩阵摘要 + 剩余 runtime-dependent 限制
- [ ] TODO.md M9-6 标 [DONE] + commit

## 完成状态（M9-6 DONE — 总体验收）
- [x] §3 parity 表 13 项逐项验收（源码 + 测试锚点）
- [x] PLAN.md §风险逐条有测试/明确限制（ACP 两条属 M10 未来）
- [x] 全部真实 e2e `#[ignore]`（10 处）；3 份 cassette is_secret_free + fixtures 凭据扫描干净
- [x] 默认 feature 无重 runtime 依赖（3 feature = []；无 agent-client-protocol）
- [x] ExternalAgentMachine 无 IO（machine.rs + machine/ 无 .await/tokio/process/fs/reqwest）
- [x] 验证序列 1-6 全过（fmt/clippy default+全feature/test-all default 753+/doc -D warnings/git diff --check）
- [x] feature-gated cassette：claude 7 / codex 7 / opencode 7
- [x] 真机 ignored e2e 全绿：claude 11.6s(6ev)/codex 59.8s(5ev)/opencode 19.1s(4ev)/mixed 188.8s(3 tests)
- [x] TODO.md M9-6 标 [DONE] + 完成记录（含最终能力矩阵摘要 + 剩余 runtime-dependent 限制）
- 下一个未完成 = M10-1（ACP feature + AcpConfig），留给下次 invocation。

---

# M10-1 增加 `external-acp` feature、ACP 依赖与 `AcpConfig` / capability 协商

**当前任务 = TODO.md 首个未完成 = M10-1**（`### [TODO] M10-1`, line 3233）。M1..M9 全 `[DONE]`。

## 任务要求（TODO.md 3233-3302）
1. `Cargo.toml`: 新增非默认 feature `external-acp`，把 `agent-client-protocol` /
   `agent-client-protocol-schema` 作 **optional** dep，只在该 feature 下启用。
   注释写明默认关闭 + 记录 ACP wire 版本（实测最新稳定版）。
2. 新增 feature-gated `src/agent/external/acp/{mod.rs,config.rs}`，在
   `src/agent/external/mod.rs` 以 `#[cfg(feature="external-acp")]` 挂载并 re-export `AcpConfig`。
3. `AcpConfig`（纯数据 serde DTO）：
   - `binary` + `args`（任意 program+args）。
   - 配置继承+注入：默认继承宿主完整 env；`env` override（BTreeMap，Debug 脱敏）；
     一个开关表达「是否继承父进程 env」（默认继承）。**绝不**承载 API key。
   - `working_dir`（worktree）。
   - `ExternalPermissionMode`（首版只存下+doc 语义；应答逻辑在 M10-3）。
   - `timeout`。
   - 预设构造器：`claude_agent_acp()`（binary=claude-agent-acp）、`codex_acp()`（binary=codex-acp）、
     `opencode_acp()`（binary=opencode, args=["acp"]）、通用 `new(binary, args)`。
4. capability 协商：纯函数 ACP initialize agent capabilities（中立投影）→ `ExternalRuntimeCapabilities`。
   保守基线 none()，只开协商位：loadSession→resume，fs/terminal 广告（记录，不等于 host_tools），
   始终 permission_bridge=true、streaming=true、graceful_shutdown=true；host_tools/host_subagents=false。
   用 `ExternalRuntimeKind::Custom("acp")` 承载。

## 验证条件（TODO.md 3287-3302）
- `AcpConfig` serde round-trip；Debug/Display 不泄漏 env secret。
- 预设构造器单测（opencode args 含 "acp"；无 API-key 字段）。
- 配置继承/注入单测（不 spawn；断言构造的 spawn env/args）：默认继承父 env；
  设 env override 后键出现；「不继承」开关下父 env 不透传。
- capability 映射纯函数单测：loadSession+fs → resume/permission_bridge/streaming true、host_tools false；
  空握手 → 只有协议保证位 true。
- 默认 `cargo build` 不拉 ACP crate；`cargo build --features external-acp` 通过。
- 聚焦测试：`acp_config_roundtrip`、`acp_capabilities_from_initialize`。
- 完整验证序列 1-6（默认 + `--features external-acp` 两配置）。

## 设计决定
- crate 版本：agent-client-protocol 1.2.0 + agent-client-protocol-schema 1.4.0（crates.io 最新稳定）。
- M10-1 只需 config + 纯 capability 映射；握手 IO 在 M10-3。为不把 crate raw 类型泄漏为 public API，
  capability 映射输入用**中立投影** struct（AcpNegotiatedCapabilities），不直接暴露 schema crate 类型。
- ACP wire 版本：需从下载的 crate 源码确认 ProtocolVersion 常量，记入注释/常量。
- 因 M10-1 不引用 crate（映射用中立投影），deps 加了但仅由 `cargo build --features` 编译验证可拉取。
  lint 仅 warn(missing_docs)，无 unused_crate_dependencies，故安全。

## 进度
- [ ] Cargo.toml: external-acp feature + optional deps + wire 版本注释
- [ ] 确认 ACP wire 版本（下载 crate 源码 grep ProtocolVersion）
- [ ] src/agent/external/acp/config.rs (AcpConfig + spawn env/args builder + 预设)
- [ ] src/agent/external/acp/mod.rs (capability 映射纯函数 + 中立投影 + re-export)
- [ ] mod.rs 挂载 + re-export
- [ ] 单测（roundtrip / 预设 / env inherit+inject / capability 映射）
- [ ] 验证序列 1-6（default + feature）
- [ ] TODO.md 标 [DONE] + commit

## 完成状态（M10-1 DONE）
- [x] Cargo.toml: external-acp feature + optional deps(1.2.0/1.4.0) + wire 版本注释(ACP_WIRE_VERSION=1)
- [x] src/agent/external/acp/config.rs（AcpConfig + resolved_env + 预设 + Debug/Display 脱敏）
- [x] src/agent/external/acp/mod.rs（AcpNegotiatedCapabilities 中立投影 + capabilities_from_initialize + 常量）
- [x] mod.rs 挂载 + re-export
- [x] 6 单测全绿（roundtrip / 预设 / debug+display 脱敏 / env inherit+inject / capability 映射 / wire version）
- [x] 连带修复 preserve_order 统一副作用：turn.rs 装箱 / LlmOutcome allow / state 测试排序 / Display intra-doc link
- [x] 验证 1-6 全过（default + external-acp 两配置）；feature 隔离 cargo tree 证据
- [x] TODO.md M10-1 标 [DONE] + 完成记录
- 下一个未完成 = M10-2（ACP client 连接 + session/update 解码），留给下次 invocation。
