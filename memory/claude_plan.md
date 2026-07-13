# 执行计划 — M1-3 `Interaction`：泛化 approval

## 当前任务

第一个未完成任务：**M1-3 `Interaction`：泛化 approval**（迁移文档 §4）。
把 yes/no 审批泛化成 interaction 子类型（审批 / 开放问题 / 选项选择）。
阶段 0 **保留并 re-export** 所有旧 approval 类型，只新增 `Interaction*` 包装，不改 `DefaultAgentLoop`。

## 约束

- 只完成 M1-3，完成后提交并停止。
- 不改现有行为；`DefaultAgentLoop` 仍用旧 approval 用法。
- 复用现有 `ApprovalRequirement`/`ApprovalResponse`，不重定义。

## 步骤

1. [done] 阅读 TODO.md M1-3、迁移文档 §4、`approval.rs`、`requirement.rs` 占位、`mod.rs`、`event.rs`。
2. 给 `ApprovalRequirement` 增加 `Serialize/Deserialize`（snake_case，纯派生，非行为变更），
   使 `InteractionKind::Approval` 可 serde。
3. 新建 `src/agent/interaction.rs`：
   - `Interaction { step_id, kind }`、`InteractionKind`（Approval/Question/Choice）、
     `InteractionResponse`（Approval/Answer/Choice）。
   - `InteractionKindTag`（Display）+ 受检 `Interaction::accepts_response`（分类 `InteractionError`）。
   - 受检构造器 `InteractionResponse::choice_for` / `approval_for` / `answer`。
   - `From<ApprovalResponse>` + `TryFrom<InteractionResponse> for ApprovalResponse`（互转无损）。
4. `requirement.rs`：删除占位 `Interaction`/`InteractionResponse`，import 真实类型；
   `RequirementKind::accepts` 对 NeedInteraction 补齐 `accepts_response` 深校验，
   新增 `RequirementError::Interaction(InteractionError)`。
5. `mod.rs`：`pub mod interaction;` + 导出 `Interaction*`；去掉 requirement 重复 re-export。
6. 聚焦测试：serde round-trip、Choice 越界拒绝、Approval id 不匹配拒绝、Approval↔旧类型互转无损。
7. 验证：fmt → clippy(-D warnings) → 聚焦 interaction 测试 → 全量测试(≤30min) → rustdoc(-D warnings) → diff check。
8. TODO.md M1-3 `[TODO]`→`[DONE]` 并补完成记录；提交并停止。

## 进度

- 已确认第一个未完成任务为 M1-3（M1-1、M1-2 已 [DONE]）。
- 占位 `Interaction`/`InteractionResponse` 仅在 `mod.rs`、`requirement.rs` 使用，替换范围可控。

## 结果（M1-3 完成）

- 已实现 `src/agent/interaction.rs`（Interaction/InteractionKind/InteractionResponse/
  InteractionKindTag/InteractionError + 受检构造器与互转），`mod.rs` 导出。
- `ApprovalRequirement` 加 serde；`requirement.rs` 占位替换为真实类型，`accepts` 深校验，
  新增 `RequirementError::Interaction`。
- fmt/clippy(-D warnings)/聚焦测试(interaction6/requirement10/approval4)/全量(lib375)/
  rustdoc(-D warnings)/diff check 全绿。
- TODO.md M1-3 → [DONE] 并补完成记录。准备提交并停止；下一轮从 M1-R 开始。
