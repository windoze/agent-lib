# M2-2 执行计划：对齐非流式和流式事件契约文档与回归测试

## 任务
TODO.md M2-2：为 `Agent::stream` 与 `Agent::run_full` 增加事件一致性对比测试，
并明确文档说明：生命周期事件（approval/tool/delegation）一致，token `TextDelta`
只属于流式路径。

## 关键发现（探针实测）
对同一 scripted 场景，两条路径的事件序列：
- auto_allow（普通 tool）：两路一致 [ToolStarted, ToolFinished]（call id 相同 ..00a）。
- ask_approve：两路一致 [ApprovalRequested, ToolStarted, ToolFinished]。
- auto_deny（分歧！）：
  - run_full：[ApprovalRequested, ToolFinished(name=<空>, id=..)]
  - stream：[ApprovalRequested]（无 ToolFinished）

分歧根因：collect_traces（src/facade/agent.rs）对被拒工具的
Notification::ToolCallFinished（没有对应 ToolCallStarted，name 查不到）也会
产出一个 ToolFinished，且 name 为空——既与流式不一致，又是空 name 的 bug。

## 决策：对齐到"被拒工具不产 tool 生命周期事件"
被拒工具从未执行 → 只应产 ApprovalRequested，不产 ToolStarted/ToolFinished。
流式路径本就如此（TapToolHandler 仅在工具真正执行时 emit）。因此修正非流式
collect_traces：当 ToolCallFinished 的 call 从未 started（names 无该 id 且非
delegation）时，跳过 ToolFinished。weave_approval_events 的尾部 flush 已保证审批
仍可见。

## 执行步骤
1. [x] 探针实测两路事件差异（已完成，探针测试待删）。
2. [x] 删除探针测试。
3. [x] 修 collect_traces：抑制被拒工具的幽灵 ToolFinished。
4. [x] 更新受影响 rustdoc/注释：collect_traces、weave_approval_events、
   tool_event_call_id（不再有"denied → ToolFinished"说法）。
5. [x] 更新 M2-1 denied 测试的过期注释，收紧为"denied 不产 ToolFinished"。
6. [x] tests.rs 增加对比测试 + lifecycle_signature helper：
   - plain tool（auto_allow）parity
   - approved tool（ask approve）parity
   - denied tool（auto_deny）parity（验证修复）
   - delegation parity（流式 supervisor + 非流式 child，dual-mode routing client）
   - 边界断言：run_full 从不含 TextDelta；stream 含 TextDelta。
7. [x] 文档：docs/facade-api.md §6 增加"事件一致性边界"说明；README.md 相应更新；
   run.rs RunEvent/RunOutput.events rustdoc 补充边界说明。
8. [x] 验证：cargo fmt / clippy --all-targets -D warnings /
   cargo test -p agent-lib --lib facade::agent:: / facade::run /
   RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace / 全量 lib 测试。
9. [x] TODO.md 标记 M2-2 [DONE] 并填完成记录；提交。

## 备注
- refine.md §3（line 172/313）本就要求 run_full/stream event parity 测试——正是本任务。
- 不改流式路径行为；只把非流式对齐到流式已有语义 + 修空 name bug（class-wide 修正）。

## 状态：完成
M2-2 已实现、验证并标记 [DONE]，等待提交。全量 `cargo test --all --all-targets` 全绿。
