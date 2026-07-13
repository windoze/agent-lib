# 执行计划

## 当前约束

- 输出和进度记录使用中文。
- `TODO.md` 是任务顺序和完成条件的唯一权威来源。
- 本轮只完成第一个标题未带 `[DONE]` 的任务，完成后提交 Git 并停止。
- 如遇阻塞，不绕过规格；在 `TODO.md` 插入最小必要前置任务并提交后停止。
- 格式化、clippy、完整测试按要求顺序执行；若只改文档且无代码变化，可复用上一轮绿色结果并记录原因。

## 本轮任务

第一个未完成任务：**M1-1 `Requirement` 与回程寻址类型**（迁移文档 §3.2/§3.3/§3.4）。
新增 `src/agent/requirement.rs`，定义 requirement/回程寻址类型骨架，不接线任何驱动逻辑，不改现有行为。

## 步骤计划

1. 阅读迁移文档 §2–§4、§12，以及现有类型（tool.rs / id.rs / approval.rs / client 类型 / LlmStepMode）。[done]
2. 新建 `src/agent/requirement.rs`：
   - `RequirementId`（Uuid 不透明 newtype，host 供给、库不生成，transparent serde）。
   - `RequirementKindTag`（Llm/Tool/Interaction/Subagent，Display）。
   - `RequirementIds` 供给 trait（`next_requirement_id(kind_tag)`）+ `NoRequirementIds`。
   - `AgentPath`(Vec<AgentSlot>) + `AgentSlot`(u32)；阶段 0 恒空路径。
   - `Requirement { id, origin: AgentPath, kind }` + `RequirementKind` 四变体
     （NeedLlm/NeedTool/NeedInteraction/NeedSubagent）。
   - 占位类型 `Interaction`/`InteractionResponse`（M1-3 替换）、`AgentSpecRef`/`SubagentOutput`（M5 细化）。
   - `RequirementResult`（运行时侧，含 ToolRuntimeError/AgentError，不要求 serde）+ `RequirementResolution`。
   - `RequirementKind::accepts(&result)`：按 tag 对齐，失败返回分类 `RequirementError`。
   - serde 边界 rustdoc：persistable 描述（serde） vs runtime 结果（非 serde）。
3. 给 `LlmStepMode` 增加 `Serialize/Deserialize`（使 `Requirement` 可序列化，非行为变更）。
4. `agent/mod.rs` 导出新模块与类型。
5. 聚焦测试：serde round-trip（Requirement/Kind/Id/AgentPath）；accepts 4×4 接受/拒绝矩阵；
   `RequirementIds` 供给失败返回分类错误（含成功用例）。
6. 验证顺序：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → 聚焦测试
   → `cargo test --all --all-targets`（≤30 分钟）→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
   → `git diff --check`。
7. 将 `TODO.md` 中 `M1-1` 标题的 `[TODO]` 改为 `[DONE]` 并补完成记录；提交并停止。

## 进度

- 已确认第一个未完成任务为 M1-1（TODO.md 全部为 `[TODO]`，git 历史里的 M1–M3 属旧任务表，已归档）。
- 已确认 `ChatRequest`/`ClientError`/`Response`/`ToolCall`/`ToolResponse` 均 serde；
  `LlmStepMode`/`ToolRuntimeError`/`AgentError` 非 serde。
- 开始实现 requirement.rs。

## 完成情况（M1-1）

- requirement.rs 实现完成，mod.rs 导出，LlmStepMode 增加 serde。
- fmt/clippy/聚焦测试(10)/全量测试(lib 367)/rustdoc/diff check 全部通过。
- TODO.md 中 M1-1 标题改为 [DONE] 并补完成记录；未改 PLAN.md（无阶段级计划变化）。
- 准备提交并停止；下一轮从 M1-2 开始。
