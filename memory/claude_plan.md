# M4-1 — 新增 `InteractionKind::Permission` 与 `PermissionRequest`

**状态:完成(已全绿,已提交)。**

## 目标(TODO.md M4-1)
- 新增 `PermissionRequest`、`PermissionCategory`、`PermissionRisk`(Low/Medium/High/Critical)类型。
- `InteractionKind` 增加 `Permission { request: PermissionRequest }`;`InteractionKindTag` 增加 `Permission`,
  更新 `tag()`/Display/所有相关 `match`。
- 增加构造器 `Interaction::permission(step_id, PermissionRequest)`。
- 现有 `Approval` 家族保持不变。

## 设计决策
- 依设计文档 §3.3 字段集:`PermissionRequest { action_id: String, actor: AgentId,
  category: PermissionCategory, summary: String, subject: serde_json::Value, risk: PermissionRisk,
  reason: Option<String> }`。
- 新建独立模块 `src/agent/permission.rs`(与 `approval.rs` 对称,interaction 复用它),避免 interaction.rs 膨胀。
- `serde_json::Value` 已实现 `Eq`(`ToolCall` 亦 derive Eq 且含 Value),故 `PermissionRequest` 可 derive Eq,
  `InteractionKind`/`Interaction` 的 `Eq` 派生不受影响。
- M4-1 仅有 request 侧,`InteractionResponse` 无 Permission 变体(M4-2 才加)。因此:
  - `Interaction::accepts_response` 的 catch-all 臂天然把任何响应对 Permission 请求判为 ResponseKindMismatch,无需改。
  - 现有 auto-responder 的 exhaustive `match request.kind()` 需补 `Permission` 臂。M4-1 没有合法 permission 响应可返,
    故补 `panic!`(与既有 `subagent/tests.rs` "never approvals" 风格一致)。reference.rs / testkit handlers.rs 的
    真实 deny-by-default 由 **M4-3** 追踪替换。测试模块内的 handler 永不收到 Permission,panic 臂长期有效。

## 需要更新的 exhaustive match
- src/agent/drive/reference.rs `ApprovalInteractionHandler::fulfill`(库,M4-3 替换)
- crates/agent-testkit/src/handlers.rs `approval_response`(testkit,M4-3 替换)
- src/agent/drive.rs test-mod `PolicyInteractionHandler`、`CountingInteractionHandler`(测试,长期 panic)
- src/agent/drive/subagent/tests.rs `CountingInteractionHandler`(测试,长期 panic)

## 步骤
1. [x] 新建 `src/agent/permission.rs`(3 类型 + 构造器 + serde round-trip 单测)。
2. [x] mod.rs 挂 `pub mod permission;` + re-export。
3. [x] interaction.rs:加 `Permission` variant / `Interaction::permission` / tag / Tag / Display / 单测。
4. [x] 补 4 处 exhaustive match 的 Permission 臂。
5. [x] fmt → clippy(-D warnings)→ `cargo test --lib permission` → `cargo test --all --all-targets` → doc → git diff --check。
6. [x] TODO.md 标 [DONE] + 完成记录;提交 `[M4-1] ...`;停止。
