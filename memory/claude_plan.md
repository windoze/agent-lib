# 执行计划 — M4-3：删除 `respond_approval`、pivot queue 残留与 `AgentFeedGuard`

## 选中的任务
`TODO.md` 第一个未完成任务 = **M4-3**（M1..M4-2a 全 `[DONE]`；M4-3/M4-R/M5.. 仍 TODO）。
前置 M4-2、M4-2a 已完成。

## 决策 E（去留）— 选择：**彻底删除 `DefaultAgentLoop` 及整个 `loop_driver` 模块**
理由：
- sans-io `AgentMachine`（machine/default）+ 参考 driver（drive.rs / drive/reference.rs）已完整取代
  loop 的自驱运行时；M3-3 起参考 driver 已按 `reference_*_matches_default_loop` 等价复跑 loop 集成测试。
- 删除 `respond_approval` + `ApprovalWaiters` 后，loop 的审批路径已无法闭合；把 loop 重构成"薄
  driver"只会重复 `drive_turn` / `ReferenceScope`。故删除是唯一无 workaround 的收尾。
- `DefaultAgentLoop` / `AgentLoop` 未被任何集成测试（tests/）或 example 依赖，仅其自身单测 +
  文档注释引用。

## 删除面
- 整个 `src/agent/loop_driver.rs`（trait `AgentLoop`、`BoxAgentLoop`、`BoxAgentEventStream`、
  `AgentEventStream`、`AgentFeedGuard`、`AgentFeedPermit`、`respond_approval` 默认方法）。
- `src/agent/loop_driver/default.rs`（`DefaultAgentLoop`、`LoopRuntime`、`ApprovalWaiters`、
  `NonStreamingSegment`、`StreamingSegment`、`LlmStepMode`）。
- `src/agent/loop_driver/default/tests.rs`（1700 行 loop 单测）。
- `AgentError::FeedInProgress` / `AgentErrorKind::FeedInProgress`（背压概念随 loop 消失）。

## 迁移/保留
- `LlmStepMode`（+ `request_stream_flag`）迁到 `requirement.rs`（它是 `NeedLlm` 的载荷类型）。
- 覆盖等价性：确认下述 loop 边界测试已被 machine/reference 覆盖，缺口补 machine 单测：
  - 已覆盖：text、streaming transport、single/parallel tool、tool 错误自愈、approval
    approve/deny/cancel、cancel 丢弃在途、client 错误丢弃 pending、reconfig（text/tool/conflict/idle）。
  - 待核实/可能补测：invalid assistant response 丢弃 pending、duplicate framework tool-call-id
    拒绝、unknown provider call 拒绝、streaming 转发。→ 逐个核对 machine 现有测试，缺则补 machine 单测。
- `AgentFeedGuard` 决策 F：随 loop 删除（`&mut self` + machine 单活 turn 已提供背压）。

## 导出/文档
- `agent/mod.rs`：删 `pub mod loop_driver;` 及 loop 相关 `pub use`；`LlmStepMode` 改从新家导出；
  更新模块级 doc（去掉 "DefaultAgentLoop 提供…"、"pivot queue" 措辞）。
- `lib.rs`：更新 crate 根 doc（去掉 `AgentLoop` 事件流契约 / `AgentFeedGuard` / `ApprovalResponse`
  loop 边界的描述，改述为 machine + driver）。
- `README.md`：去掉 `DefaultAgentLoop` 相关描述，改述 machine + 参考 driver。
- 其它 src 文件 doc 注释里对 `DefaultAgentLoop` 的 rustdoc 链接需改（否则 `-D warnings` doc 失败）。

## 验证顺序
`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` →
`cargo test --all --all-targets`（<=30min）→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` →
`git diff --check`。每测试 <1min。

## 进度
- [x] 选中 M4-3，读 TODO/迁移文档 §4/§10/§12-E/F，读 loop / machine / reference 结构
- [x] 决策 E = 删除 loop，写入本计划
- [x] 核对覆盖缺口：machine 已覆盖大多数；缺口补 3 个 machine 单测（invalid resp / duplicate id / unknown call）；streaming delta 按 §12-D 延后
- [x] 迁移 `LlmStepMode` → requirement.rs
- [x] 删除 loop_driver 模块 + 移除 FeedInProgress
- [x] 更新 mod.rs / lib.rs / README / 各 doc 链接
- [x] fmt / clippy / test（lib 423）/ doc 全绿，git diff --check 干净
- [x] TODO.md 标 [DONE] + 完成记录（写明决策 E/F 理由）+ 提交
