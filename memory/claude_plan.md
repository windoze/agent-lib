# M4-3 — interaction backend 与 testkit 的 permission 支持

**状态:完成(已全绿,已提交)。**

## 目标(TODO.md M4-3)
- 扩展 `ScriptedInteractionHandler`,对 `InteractionKind::Permission` 产出
  `InteractionResponse::Permission(..)`(approve/deny/cancel;timeout 映射为 deny-by-default),
  保持既有 approval/question/choice 行为不变。
- 参考 headless policy handler(`ApprovalInteractionHandler`,`src/agent/drive/reference.rs`)补一条
  permission 分派臂,给出 deny-by-default 安全默认。
- 在 `ExternalAgentMachine` 两段式路径中把 permission pause 走 `InteractionKind::Permission`
  (端到端 external→permission→respond)。机器本身是泛型透传 `Interaction`,故 wiring 落在 fixture:
  升级 `permission_pause()` 使其产出 `Interaction::permission`。

## 关键事实
- 机器 `pause_for_interaction`/`resume_interaction` 泛型透传 `Interaction`/`InteractionResponse`;
  `RequirementKind::accepts` → `Interaction::accepts_response` 在 drain 路径已校验 permission `action_id`。
- `PausedForInteraction` 同时带 `action_id`(runtime handle,回填 `RespondInteraction`)与
  `request: Interaction`。permission 场景下二者 action_id 需一致("act-1")。
- 现有 `approval_response`(handlers.rs)/`ApprovalInteractionHandler`(reference.rs)对
  `InteractionKind::Permission` 均 panic("... milestone 4.3"),本任务替换。
- ApprovalDecision { Approve, Deny, Timeout, Cancel } → PermissionDecision 映射:
  Approve→Approve;Deny/Timeout→Deny{reason:message};Cancel→Cancel。

## 步骤
1. [x] handlers.rs:import PermissionResponse;approval_response 的 Permission 臂产出
   InteractionResponse::Permission;新增 permission_from_approval 映射;更新 doc。
2. [x] reference.rs:import PermissionResponse;ApprovalInteractionHandler Permission 臂映射决策;更新 doc。
3. [x] fixture external.rs:新增 permission_request();升级 permission_pause() → Interaction::permission;
   更新 fixture header + 方法 doc。
4. [x] 更新 M3-3 集成测试 agent_external_interaction.rs:Answer → Approve;措辞更新。
5. [x] 新增 tests/agent_external_permission.rs:external_agent_permission_approve_flow /
   external_agent_permission_deny_flow。
6. [x] fmt → clippy(-D warnings)→ cargo test external_agent_permission → 全量 → doc → git diff --check。
7. [x] TODO.md 标 [DONE] + 完成记录;提交 [M4-3] ...;停止。
