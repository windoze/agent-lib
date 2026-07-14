# 当前任务：M7-1 设计 data-only scenario model 草案

## 定位
- `TODO.md` 第一个未完成任务 = **M7-1**（line 1383，标题 `[TODO]`）。M1..M6 全部 `[DONE]`（HEAD=8d40d57 [M6-R]）。
- 前置依赖 M6-R 已完成。无阻塞。
- 未追踪文件 `docs/external-agent.md` = 无关的 External Agent 设计草案，TODO/PLAN 未引用，不纳入本次提交。

## 任务要求（TODO.md M7-1）
做什么：
- 定义 `Scenario`、`ScenarioInput`、`ScenarioEffectScript`、`ScenarioExpectation` 数据结构草案。
- 支持 serde round-trip。
- 支持表达 text/tool/approval 三类最小场景。
- 编写一个 runner spike：scenario -> result summary。
- 明确哪些断言进入 summary，哪些仍留在 Rust assertions。

验证：
- 单测：scenario JSON round-trip。
- 单测：最小 text/tool/approval scenario 可运行。
- 全套验证命令全部通过。

## 设计（新增 `crates/agent-testkit/src/scenario.rs`）
数据模型（serde derive，reuse 公共 serde 类型 `Tool`/`Role`/`ToolStatus`/`LoopCursorKind`）：
- `Scenario { name, description?, tools: Vec<Tool>, approval: ApprovalPolicySpec, input: ScenarioInput, effects, expect }`
- `ScenarioInput`（enum，tag=kind）：`User { text }`（可扩展 pivot/external）
- `ApprovalPolicySpec`（enum）：`AutoAllow`(默认) / `RequireApproval`
- `ScenarioEffectScript { llm, tool, interaction }`
  - `ScenarioLlmStep`（tag=kind）：`Text{text,usage}` / `ToolUse{calls,usage}`；`ScenarioUsage{input,output}`；`ScenarioToolCall{id,name,input}`
  - `ScenarioToolStep`（tag=kind）：`Ok{call_id,text}` / `Error{call_id,text}`
  - `ScenarioInteractionStep`（tag=kind）：`Approve`/`ApproveWith{message}`/`Deny{message?}`/`Answer{text}`/`Choice{index}`
- `ScenarioExpectation`（全部 Option，golden 只校验设置项）：`cursor?`、`committed_turns?`、`last_assistant_text?`、`llm_calls?`、`tool_calls?`、`interaction_calls?`、`tool_results`、`message_roles`

Runner spike：
- `pub async fn run_scenario(&Scenario) -> Result<ScenarioSummary, ScenarioError>`
- `ScenarioSummary { name, cursor, committed_turns, last_assistant_text?, llm_calls, tool_calls, interaction_calls, tool_results, message_roles }`（serde Serialize）
- `impl ScenarioSummary { pub fn check(&self, expect) -> Vec<String> }`（空=通过）
- `ScenarioError::Drain(AgentError)`
- 私有 `ScenarioApprovalPolicy`（require-approval 是 spec-level，仅 runner spike 内部按数据构造，不导出）

进入 summary 的断言：committed turn 数、每 turn role 序列、last assistant text、llm/tool/interaction 计数、tool result status（按 call id）、final cursor。
仍留 Rust：trace/budget 快照、notification 细节、乱序/并发/peak、ContentBlock 内部、misaligned 注入、cancel timing/panic、requirement-id 级 step-by-step。

## 执行步骤
1. [完成] 探查 testkit/scenario 需求
2. [进行中] 新增 scenario.rs（模型 + runner + check + 内联单测）
3. lib.rs / prelude.rs 注册模块与导出
4. fmt --check → clippy -D warnings → `-p agent-testkit` scenario 测 → cargo test --all --all-targets → doc
5. TODO.md M7-1 标 [DONE] + 完成记录
6. commit [M7-1]，停止

## 完成
- [x] scenario.rs 落地 + 单测全绿（8/8）
- [x] 全套验证全绿（fmt/clippy/`cargo test --all --all-targets` 0 failed/doc）
- [x] TODO.md M7-1 [DONE] + 完成记录
- [ ] commit [M7-1] 停止 ← 进行中
