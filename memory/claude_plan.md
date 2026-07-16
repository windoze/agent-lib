# M3-4 — cancel/abandon 清理、shutdown disposition 与挂载验证

**状态:完成(已全绿,待提交)。**

## 目标(TODO.md M3-4)
- `step(Abandon(id))`:never-resume 关闭,cursor 收敛到可复用终止态并标注外部 session 清理,不 emit 新 requirement。
- 明确清理归属:handle 层(`ExternalRuntimeHandles` Drop / 容器 teardown)负责关进程,machine 不 emit `Shutdown`。
- 定义 shutdown disposition 小枚举并在 trace 体现。
- 验证 `ExternalAgentMachine` 能作为 child 经 `NeedSubagent` 派生驱动。

## 关键设计决策
- `ExternalSessionShutdown` 必须 `Copy`(`TraceNodeKind` 派生 `Copy`),故为 C-like 枚举;详细失败文本留在
  `ExternalAgentError::ShutdownFailed`。
- machine 是 sans-io,abandon 只能在 state 上 `mark_cleanup_required()`,真正 kill + disposition 记录在 handle 层。
- `NestedMachine.own` 是具体 `DefaultAgentMachine`、子槽是 `NestedMachine`,外部 machine 不能字面做 slot child;
  走 `SpawnedChild.machine: Box<dyn AgentMachine>` + `DrivingSubagentHandler` + `ScriptedSubagentSpawner` 的嵌套 drain 路径。
- drain cancel 路径(`src/agent/drive.rs` ~436):cancel 后记 `NeverResumed` + `step(Abandon)` 再 break;
  集成 abandon 测试即「先 cancel ctx 再 run_user」。

## 落地清单(全部完成)
- [x] `src/agent/external/shutdown.rs`(新)+ mod/re-export
- [x] trace:`TraceNodeKind::ExternalShutdown` + `record_external_shutdown` + testkit describe_kind + context 测试
- [x] state:`cleanup_required` 字段 + 三方法 + serde(default/skip)+ 单元测试
- [x] machine:`abandon` 重写 + rustdoc + 模块头文档
- [x] runtime:`ExternalRuntimeHandles` 清理归属 rustdoc
- [x] machine 单元测试:重写 abandon 测试 + 2 个新增
- [x] 集成测试 `tests/agent_external_lifecycle.rs`(abandon + mounts)
- [x] `docs/external-agent.md` §6.4 更新
- [x] 验证门:fmt / clippy / 焦点测试 / 全量 672 passed / doc 全绿
- [x] TODO.md 标 [DONE] + 完成记录

## 下一步
提交(`[M3-4] ...` + Co-authored-by 尾注),然后停止,不启动 M3-5。
