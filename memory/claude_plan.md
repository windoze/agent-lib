# 执行计划 — M6-1：更新主文档与 PLAN/TODO 交叉引用

## 选中的任务
`TODO.md` 第一个未完成任务 = **M6-1**（M1..M5-R 全 `[DONE]`）。文档并轨任务，不拆分。
前置 M5-R 已 `[DONE]`。工作树开始时 clean，HEAD=68dfcc3（M5-R）。

## 任务要求（TODO.md M6-1）
1. 改写 `docs/agent-layer.md` §1.3（feed→stream → step→requirements pull 契约）、
   §3/§4（审批/pivot/cancel 从三种并列机制 → 同一 requirement+handler 的三种表现）。
2. 更新 `docs/agent-effect-model.md` 与 `docs/agent-effect-migration.md` 顶部状态标注为“已落地”，
   链接到实现位置（`agent/machine`、`agent/drive`、`agent/requirement` 等）。
3. 更新 crate 根文档（`src/lib.rs`）与 `README.md`，纳入 sans-io + effect-handler 模型，
   移除已删 push API 描述。

## 现状核对（已读代码）
- 实现落位：sans-io 契约在 `src/agent/machine/mod.rs`（`AgentMachine::step`/`StepInput`/`StepOutcome`），
  具体机在 `src/agent/machine/default/`、嵌套树在 `src/agent/machine/nested.rs`；effect handler/drain/pop
  在 `src/agent/drive.rs`（`HandlerScope` + 四个 handler trait + `drain`/`drive_turn`/`Pop`），
  subagent handler 在 `src/agent/drive/subagent.rs`；requirement 寻址在 `src/agent/requirement.rs`
  （`Requirement`/`RequirementKind`{NeedLlm,NeedTool,NeedInteraction,NeedSubagent,NeedReconfigRegistry}/
  `RequirementId`/`AgentPath`）；Notification/AgentInput 在 `src/agent/event.rs`。
- 旧 push API（`AgentLoop::feed`/`interject`/`respond_approval`/`AwaitingApproval`/`AgentEvent::Done`/
  pivot queue/`AgentFeedGuard`）在代码里已删除。`README.md`/`src/lib.rs` 已基本反映新模型；
  `docs/agent-layer.md` 仍描述旧 push `feed → AgentEvent stream` 契约，是本任务主要改写对象。

## 步骤
1. 改写 `docs/agent-layer.md`：顶部 banner、§1 组合图、§1.3、§2（feed 提及）、§3 表、§4、§8/§9
   与新模型一致；加实现链接。
2. 改写 `docs/agent-effect-model.md`、`docs/agent-effect-migration.md` 顶部状态块为“已落地”+实现链接。
3. 核对/补齐 `src/lib.rs`、`README.md`，确保无 push API 残留、含 effect-handler/pop 说明。
4. 验证：`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`、`cargo fmt --all`、
   `cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`。
5. 标记 M6-1 `[DONE]` + 完成记录，提交，停止。

## 进度
- [x] 步骤 1  - [x] 步骤 2  - [x] 步骤 3  - [x] 步骤 4  - [x] 步骤 5（提交中）
