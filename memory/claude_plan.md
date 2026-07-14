# 当前任务：M4-1 实现 `StepHarness`

## 目标（来自 TODO.md M4-1）
在 `crates/agent-testkit/src/harness.rs` 实现同步（非 async）的 `StepHarness<M: AgentMachine>`：
- 支持 `external(input)`、`user(text)`、`pivot(...)`、`resume(id, result)`、`abandon(id)`。
- 每步返回 `StepObservation`：notifications、requirements、quiescent、cursor snapshot。
- convenience extractor：`single_llm`、`single_tool`、`single_interaction`、`requirements_by_tag`。
- 错误/断言失败信息包含：当前 cursor、outstanding ids、最近一步 label。

## 验证要求
- 单测：用 `DefaultAgentMachine` 跑 text-only turn step-by-step。
- 单测：wrong id resume 失败信息包含 cursor / outstanding id。
- 单测：`StepHarness` 本身不使用 async（普通 `#[test]` 即可证明）。
- 全套验证命令：fmt → clippy -D warnings → 聚焦测试 → 全量测试 → rustdoc → git diff --check。

## 设计
- `StepHarness<M>`：持有 machine、`SeqIds`（供 user/pivot 造 input）、`outstanding: BTreeMap<RequirementId, Requirement>`、`last_label: Option<String>`。
  - `new(machine)` / `with_ids(machine, ids)`。
  - `external/user/pivot`：infallible，step 后 ingest 新 requirements，记录 label。
  - `resume/abandon`：先在 harness 层校验 id 是否 outstanding + `accepts_resolution` 对齐，失败返回 rich `StepHarnessError`（含 cursor/outstanding/label），成功则 step、移除该 id、ingest。
  - 提供 `try_resume/try_abandon` 返回 `Result`，`resume/abandon` 为 `.unwrap_or_else(panic)` 包装（happy-path 用）。
- `StepObservation`：label、notifications、requirements、quiescent、cursor(LoopCursor clone)。
  - `requirements_by_tag(tag)`、`single`、`single_llm/tool/interaction`，失败返回 `StepHarnessError`（cursor 来自 observation、outstanding=本步 requirement ids、label=本步 label）。
- `StepHarnessError`：message + cursor(LoopCursorKind) + outstanding(Vec<RequirementId>) + last_label；Display 包含全部三项；实现 `std::error::Error`。

## 步骤
1. [x] 读 TODO/PLAN/machine/requirement/event/cursor/fixtures/scope，确认 API。
2. [x] 实现 harness.rs（StepHarness / StepObservation / StepHarnessError + 单测）。
3. [x] prelude.rs 增加再导出。
4. [x] cargo fmt --all。
5. [x] cargo clippy --all-targets -- -D warnings。
6. [x] 聚焦测试：cargo test -p agent-testkit harness。
7. [x] 全量：cargo test --all --all-targets（有代码变更，需跑）。
8. [x] RUSTDOCFLAGS="-D warnings" cargo doc --no-deps。
9. [x] git diff --check。
10. [x] TODO.md 标 [DONE] + 完成记录。
11. [x] 提交(进行中)并停止。

## 备注
- 无阻塞 spec 偏差；未发现未排期失败测试。
- docs/external-agent.md 为无关未跟踪文件，不纳入本任务提交。
