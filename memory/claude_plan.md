# M4-2 — 扩展 `InteractionResponse` 的 permission 响应并校验

**状态:完成(已全绿,已提交)。**

## 目标(TODO.md M4-2)
- 新增 `PermissionDecision { Approve, Deny { reason: Option<String> }, Cancel }`(不绑定 `ToolCallId`)。
- 新增 `PermissionResponse { action_id: String, decision: PermissionDecision }`。
- `InteractionResponse` 增加 `Permission(PermissionResponse)` 变体,更新 `tag()`。
- `Interaction::accepts_response`:`Permission` 请求只接受 `Permission` 响应,且 `action_id` 必须匹配。
- `RequirementKind::accepts` 的 `NeedInteraction` 家族校验:已通过 `accepts_response` 委派,自动生效;补测试确认。

## 设计决策
- 响应侧类型放入 `src/agent/permission.rs`(与 `approval.rs` 同时含 request/response 对称)。
- `PermissionDecision`:externally-tagged enum,`rename_all=snake_case`;`Deny.reason` 用 `#[serde(default, skip_serializing_if)]`;`deny` 空 reason 归一化为 None。
- `PermissionResponse`:`deny_unknown_fields`;构造器 `new/approve/deny/cancel` + accessor `action_id()/decision()`。
- `accepts_response` 新增臂:`(Permission{request}, Permission(resp))` => 校验 `resp.action_id()==request.action_id()`,否则 `InteractionError::ActionMismatch`。
- 新增 `InteractionError::ActionMismatch { expected: String, actual: String }` 含 String 字段 → 必须去掉 `InteractionError` 的 `Copy` 派生 → 连带去掉 `RequirementError`(含 `#[from] InteractionError`)的 `Copy`。已确认无处依赖二者的 Copy 语义(仅在 Result 中移动)。
- 新增便捷构造器 `InteractionResponse::permission_for(interaction, response)`,与 `approval_for`/`choice_for` 对称。

## 步骤
1. [x] permission.rs:加 `PermissionDecision`、`PermissionResponse` + 构造器 + 单测。
2. [x] mod.rs re-export `PermissionDecision`、`PermissionResponse`。
3. [x] interaction.rs:import、`InteractionResponse::Permission`、`tag()`、`accepts_response` Permission 臂、`permission_for`、`InteractionError::ActionMismatch`、去 Copy、单测。
4. [x] requirement.rs:`RequirementError` 去 Copy;补 permission accepts 对齐测试。
5. [x] fmt → clippy(-D warnings)→ `cargo test --lib permission_response` → `cargo test --all --all-targets` → doc → git diff --check。
6. [x] TODO.md 标 [DONE] + 完成记录;提交 `[M4-2] ...`;停止。
