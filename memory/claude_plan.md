# M9-4 增加真实 DeepSeek 父协调器 + Claude Code/Codex 子 agent ignored e2e

**当前任务 = TODO.md 首个未完成 = M9-4**（`### [TODO] M9-4`, line 2861）。M1..M9-3 全 `[DONE]`。

## 任务要求（TODO.md 2861-2896）
- 新增 ignored integration test：`tests/agent_external_managed_real_e2e.rs`。
- 启动前：从 `.envrc`/env 读 DeepSeek endpoint/key/model（不打印 secret）；probe Claude Code/Codex binary+登录态；
  检查 feature flags；缺失时 skip 或返回明确 non-secret 错误。
- 结构：DeepSeek 父协调器 LLM；父 agent 通过 `NeedSubagent` 派生 Claude Code child + Codex child；
  至少一个 child 触发 **managed interaction 或 tool bridge**；父协调器合并两 child 结果给 summary。
- 临时/隔离 worktree，结束 cleanup。
- 验证：默认 `cargo test --all --all-targets` 不运行真实 e2e；ignored 命令文档化；
  完整验证序列 1-6 全过；真实 e2e 单独记录，不纳入默认验证。

## 与既有 `tests/agent_external_real_e2e.rs` 的差别（关键）
既有文件用**手搓 CLI runner**（`CliSessionHandler` → `run_claude_code`/`run_codex` 直接 spawn `claude`/`codex exec`）。
M9-4 要的是 **managed** 路径：child 用 M6/M7 的真实 `ClaudeCodeAdapter`/`CodexAdapter`（经
`ExternalSessionRegistry` + registry-backed `ExternalSessionHandler`）驱动，走结构化 stream-json 解码、
sequenced observations、managed permission bridge。所以新文件 feature-gated `external-claude-code external-codex`。

## 已核对的 API
- registry-backed handler 模板：testkit `ScriptedRuntimeExternalSessionHandler::advance`
  = `registry.get_or_start(request, ctx, Some(sink)).await` → `handle.lock().await.advance(&request.input, ctx)` →
  `RuntimeDecisionPoint`→`ExternalSessionResult`（`From<Result<RuntimeDecisionPoint, Err>>`）。
- adapter 构建：`ClaudeCodeConfig::new().with_binary().with_working_dir().with_permission_mode(Prompt).with_timeout()`
  （+ 可选 `with_model`）→ `probe(&config).await` → `ClaudeCodeAdapter::with_probed_capabilities(config, &probed)`
  → `ExternalSessionRegistry::new(Arc::new(adapter))`。Codex 同构（`codex_probe`）。adapter CWD 取 `config.working_dir()`。
- session cleanup：`registry.cleanup_agent(agent_id)`；两 adapter `kill_on_drop(true)` 兜底。
- child ExternalAgentMachine：`ExternalAgentSpec::new(agent_id, runtime, WorktreeRef, None, ToolSetRef, policy)`
  → `ExternalAgentState::new(spec, Conversation)` → `ExternalAgentMachine::new(state, Arc<SeqIds>)`。
- 子 scope：`.external(managed_handler).interaction(ScriptedInteractionHandler::approve_all())`。
  permission pause → machine 复现 `NeedInteraction` → approve_all 批准 → 回灌 `RespondInteraction`（cassette 测试已证）。
- 父：`DeepSeekLlmHandler`（复用既有 reqwest + DeepSeek 协议）+ `DrivingSubagentHandler(spawner, depth)`。
- `SubagentSpawner::spawn` 是 **sync**，probe 是 **async** → 必须在测试 setup 先 probe 建好两个 adapter/handler，
  spawn 里只 clone 预建 handler Arc + 新建 interaction handler + 建 child machine/scope。

## 实施
新文件 `tests/agent_external_managed_real_e2e.rs`（顶部 `#![cfg(all(feature="external-claude-code", feature="external-codex"))]`，
features 关时整文件 cfg 掉 = 空 crate）：
1. E2eEnv + `.envrc` 解析（复用既有约定；不打印 secret）。
2. `DeepSeekConfig`/`DeepSeekLlmHandler`（协调器 LLM）。
3. `RecordingSink`（`ExternalEventSink` 计数）+ `ManagedSessionLog`（记录 runtime→summary/observation count）。
4. `ManagedRuntimeHandler`（registry-backed，实 adapter）。
5. `ManagedExternalSubagentSpawner`（持预建 claude/codex handler；spawn 建 child machine+scope；summarize 取 log）。
6. `DeepSeekCoordinatorMachine`（plan→NeedSubagent(claude)→NeedSubagent(codex)→final synth）。
7. 三个 `#[ignore]` 测试：claude-only managed、codex-only managed、DeepSeek 协调 mixed。
   缺 DeepSeek/binary/probe → skip（eprintln + return）。
   Claude child prompt 要求在 Prompt 模式下写文件 → 触发 managed permission interaction（approve_all 记录）。
8. 硬断言（对齐验证 1-6 清单）：coordinator Done；claude+codex 都 spawned+completed；至少一次 observation replay；
   DeepSeek 调用≥2；final text 含 FINAL_MARKER 且父 conversation committed。managed interaction 数记录+打印。
9. 文件模块 doc 写清运行命令、feature flags、env、worktree isolation、secret redaction。

## 验证序列（本任务无 src 改动，仅新增 feature-gated ignored 测试）
1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --features "external-claude-code external-codex" -- -D warnings`（新测仅 features 下编译）
3. 编译新测：`cargo test --features "external-claude-code external-codex" --test agent_external_managed_real_e2e -- --list`
   （确认 ignored 测试且不运行真实调用）
4. `cargo clippy --all-targets -- -D warnings`（默认）
5. `cargo test --all --all-targets`（默认；新测 cfg 掉=空 crate，真实 e2e 不跑）
6. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` + `git diff --check`

真实 e2e 本机无 DeepSeek/Claude/Codex，无法执行 → 完成记录标注「按环境缺失跳过，命令已文档化」。

## 进度
- [x] 写 test 文件 `tests/agent_external_managed_real_e2e.rs`（3 个 `#[ignore]` 测试，feature-gated）
- [x] 验证序列 1-6 全过（fmt / clippy±features / --list=3 ignored / 默认 clippy / 默认 `cargo test --all --all-targets` 47 bin 全 ok 0 failed / doc + git diff --check 干净）
- [x] TODO.md 标 [DONE] M9-4 + 完成记录（line 2861 起）
- [ ] commit（`[M9-4] Add managed DeepSeek+Claude Code/Codex ignored real e2e`）→ 然后 STOP
