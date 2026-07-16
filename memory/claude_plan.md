# 当前任务：M1-1 搭建外部 CLI 低保真 spike

## 定位
- `TODO.md` 首个未完成任务 = **M1-1**（第 31 行，`[TODO]`）。全表均 `[TODO]`，这是新一轮「External Agent 接入」任务单的第一项。
- 工作树干净，HEAD=b1e0388。上一轮（complex mock tests）已归档。

## 关键约束
- spike 只用于观察，**不进 `src/`**；`git diff --check` 必须干净（除新增文件外不动核心库）。
- `.gitignore` 忽略 `/probes`，因此可提交的 spike 放 `examples/`（examples 属 root package，会被 clippy --all-targets 编译）。
- 用 stub 脚本（`sh -c`）占位外部 CLI，不接真实 Claude Code/Codex，不依赖网络/credentials。
- 必须覆盖三条行为：正常启动返回文本、流式增量读取、进程取消/kill（观察 `RunContext::is_cancelled`）。

## 使用的公开 API（均已确认导出）
- `agent_lib::agent::{LlmHandler, LlmStepMode, RequirementResult, RunContext, RunId, BudgetLimits, TraceNodeId, CancellationToken}`
- `agent_lib::client::{ChatRequest, Response, ClientError}`
- `agent_lib::model::{content::ContentBlock, message::{Message, Role}, normalized::StopReason, usage::Usage}`
- `RequirementResult::Llm(Result<Response, ClientError>)`
- `RunContext::new_root(RunId::new(uuid), BudgetLimits::unbounded(), TraceNodeId::new("..."))`
- `ctx.cancellation().clone()` → `CancellationToken`（Clone，可在 spawn 任务里 `.cancel()`）

## 实现方案
`examples/external_cli_spike.rs`（自包含，无需外部 stub 文件）：
- `ExternalCliLlmHandler { program, args }` 实现 `LlmHandler`：
  - spawn 子进程（`tokio::process::Command`，`kill_on_drop(true)`，stdout piped）。
  - reader task 读 stdout 行 → mpsc（`recv()` cancel-safe）。
  - 主循环 `tokio::select!`：mpsc 收增量（Streaming 模式打印增量）；`sleep(10ms)` 轮询 `ctx.is_cancelled()`，取消时 `child.start_kill()` + `wait()`，返回 `Err(ClientError::Other("cancelled..."))`。
  - EOF 后 fold 成 `Response`（StopReason end_turn，Usage 估算 token 数）。
- stub 命令：`sh -c` 脚本按参数打印 N 个 chunk，每个 `sleep d`；prompt 经 env `SPIKE_PROMPT` 透传。
- `main()` 顺序演示：
  1. 正常/非流：少量 chunk，快速返回，打印 folded 文本 + usage。
  2. 流式：Streaming 模式，逐行打印增量。
  3. 取消：长跑 stub（多 chunk + sleep），spawn 任务在 ~150ms 后 `token.cancel()`，观察子进程被 kill 且返回 Err。
- 不加 `#[test]`，避免 `cargo test` 执行子进程。

## 验证门（M1-1 要求）
1. `cargo fmt --all -- --check`
2. `cargo run --example external_cli_spike`（跑通三条行为）
3. `cargo clippy --all-targets -- -D warnings`
4. `git diff --check`（且 `git status` 显示 src/ 无改动）

## 完成
- TODO.md M1-1 `[TODO]`→`[DONE]` + 完成记录；提交 `[M1-1] ...`；停止。

## 进度
- (进行中) 已定位 M1-1，核对全部所需公开 API，开始写 example。

## 进度更新（M1-1 完成）
- 新增 `examples/external_cli_spike.rs`;三条行为经 `cargo run --example external_cli_spike` 实测跑通。
- fmt/clippy 均通过;`src/` 无改动;`git diff --check` 干净。
- TODO.md M1-1 标 [DONE] 并写完成记录（M1-2 heading 已确认完好)。
- 下一步:提交 [M1-1],停止;下轮从 M1-2 继续。

---

# 当前任务：M1-2 记录 spike 结论（doc-only）

## 定位
- `TODO.md` 首个未完成任务 = **M1-2**（第 90 行 `[TODO]`）。M1-1 已 `[DONE]` 且已提交（HEAD=320dcb4）。
- 纯文档任务：在 `docs/external-agent.md` §15 之后新增「附录 A：Phase 0 spike 结论」。

## 做什么
- 记录 spike 四类实测结论：启动方式 / 流 decoder 形态 / 取消行为 / 成本量级，及对后续 DTO/handler 设计影响。
- ≥3 条对 Milestone 2（M2-1..M2-5）的具体、可操作影响。
- 对与文档不一致处以「(spike 修正)」脚注标注（§3.1 fold 有损、§5.5 行解码不足），不删原文。

## 依据
- spike 源码 `examples/external_cli_spike.rs`（reader task + mpsc、tokio::select! 轮询 is_cancelled、start_kill+wait、fold 词数估算 usage、env SPIKE_PROMPT 透传 prompt）。
- 相关章节：§3.1、§4.3、§5.5、§6.4、§14。

## 验证门
- Markdown 无断链；`git diff --check` 干净。
- 仅改文档（无 src/ 变更），可复用上次全量测试绿；仍跑 fmt/clippy 无害快速校验（可选）。

## 进度
- (进行中) 已读 spike 源码与 §3.1/§5.5/§6.4/§13/§14/§15，开始写附录。

## 进度更新（M1-2 完成）
- `docs/external-agent.md` §15 后新增「附录 A：Phase 0 spike 结论」（A.1 启动/A.2 decoder/A.3 取消/A.4 成本 + A.5 对 M2 的 5 条可操作影响 + A.6 go/no-go=Go）。
- §3.1、§5.5 各加「(spike 修正)」引用块，未删原文。
- TODO.md M1-2 标 [DONE] 并写完成记录；M1-3 heading 完好。
- 纯文档改动、`git diff --check` 干净、无断链；复用上次全量绿，跳过 cargo test。
- 下一步：提交 [M1-2]，停止；下轮从 M1-3 继续。

## 进度更新（M1-2 完成）
- docs/external-agent.md §15 后新增「附录 A：Phase 0 spike 结论」（A.1 启动/A.2 decoder/A.3 取消/A.4 成本 + A.5 对 M2 的 5 条可操作影响 + A.6 go/no-go=Go）。
- §3.1、§5.5 各加「(spike 修正)」引用块，未删原文。
- TODO.md M1-2 标 [DONE] 并写完成记录；M1-3 heading 完好。
- 纯文档改动、git diff --check 干净、无断链；复用上次全量绿，跳过 cargo test。
- 下一步：提交 [M1-2]，停止；下轮从 M1-3 继续。

---

# 当前任务：M1-3 Milestone 1 Review

## 定位
- `TODO.md` 首个未完成任务 = **M1-3**（第 131 行 `[TODO]`）。M1-1/M1-2 均 `[DONE]` 且已提交。
- HEAD=4a7bde4，工作树干净。这是一个 review 任务（`*Review`），**不拆分**。

## 前置核对（已完成）
- M1-1 提交 320dcb4 仅动 examples/external_cli_spike.rs + TODO.md + memory；M1-2 提交 4a7bde4 仅动 docs/external-agent.md + TODO.md + memory。`src/` 自 M1 起未改（最后 src 改动为 d8c2d9a README 重写，早于 M1）。
- spike 位于 `examples/`（非 src/），符合「不进核心库」。

## 做什么（review）
1. 核对 spike 只在 examples/、src/ 无改动、附录 A 结论完整。
2. 确认三条行为（启动/流/取消）有可复现入口：`cargo run --example external_cli_spike`。
3. 记录进入 Milestone 2 的前置结论（黑盒 fallback 取舍、取消清理最小要求、go/no-go）。

## 验证门（M1-3 明确要求全量）
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all --all-targets`（**必须真跑**：M1-1 新增了 example 但当时只跑了 fmt+clippy+run，未跑全量；M1-2 纯文档。故 example 尚未经全量套件校验。）

## 完成
- TODO.md M1-3 `[TODO]`→`[DONE]` + 完成记录（写明 M2 go/no-go 与调整项）。
- 提交 `[M1-3] ...`；停止。

## 进度
- (进行中) 已定位 M1-3、核对 M1-1/M1-2 提交范围，开始跑验证门。

## 进度更新（M1-3 完成）
- 全量验证门本轮真跑并全绿：fmt 无差异；clippy 无告警；`cargo test --all --all-targets` 0 failed（lib 423 + testkit 131 + 集成/doc/replay，仅 4 个 credential-gated ignored）。
- `cargo run --example external_cli_spike` 三条行为（启动/流/取消）跑通，入口可复现。
- 核对：spike 仅在 examples/，src/ 自 M1 起零改动；附录 A 完整；§3.1/§5.5 spike 修正标注在。
- TODO.md M1-3 标 [DONE]，写完成记录（含 M2 go/no-go=Go、黑盒 fallback 取舍、取消清理最小要求、成本字段结论）。M2-1 heading 完好。
- 下一步：提交 [M1-3]，停止；下轮从 M2-1 继续。
