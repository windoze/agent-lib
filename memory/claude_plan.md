# M4-4 — Milestone 4 Review

**状态:完成(已全绿,已提交)。**

## 目标(TODO.md M4-4)
- 核对 permission 泛化端到端自洽,未破坏既有 approval 语义:
  1. `InteractionKind` / `InteractionKindTag` / `InteractionResponse` 三处新增对齐。
  2. `validate_response`(= `Interaction::accepts_response`)与 `RequirementKind::accepts` 双重校验一致。
  3. deny-by-default 安全默认到位。
  4. 设计文档 §3.3/§14 permission 字段集与实现一致;回填「最小字段集」结论。

## 审阅结论
1. [x] 三处新增对齐:Kind/Response/Tag 各含 Permission 臂,tag() 互映,Display 渲染 "permission"。
2. [x] 双重校验一致:drive::validate → RequirementKind::accepts(type 层)→ Interaction::accepts_response
       (family + action_id 层)。互不重复,单测双向覆盖。
3. [x] deny-by-default:两 backend 共用 Approve/Deny+Timeout/Cancel 映射(timeout→deny);headless
       ApprovalInteractionHandler::deny(..) 即安全默认。
4. [x] 文档回填:§3.3 增补审批结果 shape + 最小字段集结论;§14 未定问题标注「已定(Milestone 4)」。

## 验证(完整序列全绿)
- fmt --check 无差异;clippy -D warnings 0 告警;external_agent_permission 2 过;
  cargo test --all --all-targets 全绿(lib 467、testkit 136、集成 0 failed);doc -D warnings 0 告警;
  git diff --check 干净。
- 本任务仅改文档/TODO/PLAN,未改编译代码,全量结果与 M4-3(3e82f1c)一致。

## 交付
- docs/external-agent.md §3.3/§14 回填;TODO.md M4-4 标 [DONE] + 完成记录。
- 提交 [M4-4] Milestone 4 review。停止。
