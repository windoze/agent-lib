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
