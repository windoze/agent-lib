# 执行计划 — M4-2a：迁移 turn-boundary reconfig 应用进 sans-io 机器（含 registry 解析 effect）

## 选中的任务
`TODO.md` 第一个未完成任务 = **M4-2a**（M1..M4-2 全 `[DONE]`；M4-3/M4-R/M5-1 仍 TODO）。
目标：把 legacy loop 独有的 turn-boundary reconfig 应用迁移进 sans-io 机器 + 参考 driver，
含把 registry 解析 reify 为 `NeedReconfigRegistry` effect，为 M4-3 删除 loop 扫清阻碍。
不删除 loop（保留其 reconfig 实现作对照到 M4-3）。

## 关键设计决策

### 1) 新 requirement family `Reconfig`
- `RequirementKindTag::Reconfig`
- `RequirementKind::NeedReconfigRegistry { tool_set: ToolSetRef }`（可序列化）
- `RequirementResult::Reconfig(Result<(), ToolRuntimeError>)`（运行期，driver 侧换 registry 副作用）
- 更新 tag()/accepts()/NoRequirementIds + requirement.rs 全部穷尽 match + 测试矩阵 ALL_TAGS。

### 2) 新 cursor `AwaitingReconfig(ReconfigCursor)`
- `ReconfigCursor { step_id: Option<StepId>, requirement: Option<CursorRequirement> }`
- 合法迁移：Idle→AwaitingReconfig、StreamingStep→AwaitingReconfig、
  AwaitingReconfig→{StreamingStep, Done, Error, CancelRecovery}。
- 更新 kind/validate/pending_requirement_ids/can_transition_to + loop 穷尽 cursor match。

### 3) reconfig 边界记录/元数据 helper 抽到 state/queue.rs 共享
- `reconfig_boundary_records(&[ReconfigRequest]) -> Vec<Value>`
- `reconfig_boundary_metadata(Vec<Value>) -> Map`
- loop 改用共享 helper，删本地 reconfig_records/reconfig_record/reconfig_metadata。

### 4) 机器 machine/default/mod.rs
- 持 Arc<dyn ToolRegistryResolver>（默认 DeclaredOnly，builder with_tool_registry_resolver），仅 host 用。
- reconfigure(&mut self, ReconfigRequest) -> Result<(), AgentError>：plan_reconfig_with →
  resolve+校验 declarations → queue_prevalidated_reconfig（镜像 LoopRuntime::queue_reconfig）。
- 边界应用：begin_user_turn + commit_text_turn 两处，tool set 不变直接 apply（begin 无 metadata，
  commit 写 metadata）；tool set 变 → emit NeedReconfigRegistry park 在 AwaitingReconfig，
  scratch pending_reconfig 存续；resume_reconfig 校验 Reconfig(Ok) 后分派。
- abandon(AwaitingReconfig)=丢 pending+清 scratch+finish_cancel。

### 5) 参考 driver drive.rs + drive/reference.rs
- ReconfigHandler trait + HandlerScope::reconfig() + drain 分支。
- ReferenceScope 加 resolver + 可交换 registry（Arc<Mutex<Arc<dyn ToolRegistry>>>），
  reconfig handler resolve+校验+换 registry 返回 Reconfig(Ok/Err)；builder with_tool_registry_resolver。

### 6) 测试（加到 drive/reference/tests.rs；loop 3 个 reconfig 测试保留到 M4-3）
- during-text-turn（手动 step）、during-tool-turn（手动 step）、conflicting 原子拒绝、
  idle-queued（drive_turn）、driver registry swap 端到端执行。

## 验证
cargo fmt --all → cargo clippy --all-targets -- -D warnings → cargo test --all --all-targets
→ RUSTDOCFLAGS="-D warnings" cargo doc --no-deps → git diff --check。每测试 <1min。

## 进度
- [x] 选中 M4-2a，完成代码/测试深读与设计
- [x] requirement.rs：Reconfig family
- [x] cursor.rs：AwaitingReconfig
- [x] queue.rs：共享 metadata helper + loop 改用
- [x] machine：resolver/reconfigure/边界应用/resume/abandon
- [x] driver：ReconfigHandler + ReferenceScope 换 registry
- [x] 迁移/新增 reconfig 测试（machine 3 + reference 2，全绿）
- [x] fmt/clippy/test(438)/doc 全绿
- [x] TODO.md 标 [DONE] + 完成记录 + 提交
