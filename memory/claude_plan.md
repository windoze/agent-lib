# 执行计划 — M1-2 deterministic id source `SeqIds`

## 选中的任务
`TODO.md` 第一个未完成任务 = **M1-2**(M1-1 已 `[DONE]`)。HEAD=27f240f,工作树 clean。
非 Review 任务,不拆分;仅改动 testkit,无 agent-lib 语义变更。

## 目标(TODO.md M1-2)
在 `crates/agent-testkit/src/ids.rs` 实现:
1. `SeqIds { counter: Arc<AtomicU64>, base/prefix, label }`,clone 共享 counter 保证不重复。
2. impl `RequirementIds`(`next_requirement_id`)与 `ToolExecutionIds`(4 个方法)。
3. inherent helpers:requirement_id/run_id/trace_node(label)/agent_id/tool_set_id/
   conversation_id/turn_id/message_id/tool_call_id/step_id。
4. `fork(label)`(新 base 子树,共享 counter)与 `named(label)`(同 base 重贴 label)生成可读 label
   的 trace id,底层仍唯一。
5. 分配日志:记录 requirement id 按 `RequirementKindTag` 的分配顺序,可按 tag 查询。
6. 耗尽/失败模式:`exhausted()` / `with_budget(n)`,trait 方法返回 IdUnavailable。
7. prelude re-export `SeqIds`。

## 设计
- UUID 组成:`(base as u128) << 64 | seq`,seq 取自共享 `AtomicU64`(从 1 起,永不为 nil),
  全局单调唯一 → 跨 clone/fork 不冲突;base 只做高位可读区分子树。
- 共享内部 `Arc<Shared> { counter, requirement_log: Mutex<Vec<(tag,id)>>, remaining: AtomicI64(-1=unlimited), base_counter }`。
- clone 共享 Shared+base+label;`fork` 共享 Shared、新 base(base_counter.fetch_add)、嵌套 label;
  `named` 共享 Shared+base、新 label。
- trace_node → `TraceNodeId(format!("{label}:{node}#{seq}"))` 唯一且可读。
- 失败模式:`remaining` CAS 递减,0 时 trait 方法返回 `RequirementError::IdUnavailable` /
  `ToolRuntimeError::IdUnavailable`;inherent helpers 不受限(构造 fixtures 用)。

## 步骤
1. [ ] 写 ids.rs(带模块/项文档,#![warn(missing_docs)] 已在 lib)。
2. [ ] prelude re-export SeqIds(及 RequirementAllocation 如需要)。
3. [ ] 单测:clone 共享 counter 不重复;fork 唯一;ids 可被 agent-lib 解析/使用;
       失败模式两种错误;log 按 tag 查询。
4. [ ] fmt → clippy -Dwarnings → test -p agent-testkit → test --all --all-targets → doc -Dwarnings → diff --check。
5. [ ] TODO.md 标 M1-2 [DONE] + 完成记录。
6. [ ] 提交,停止。

## 进度/发现
- (进行中)

## 完成(M1-2)
- ids.rs 落地 SeqIds(共享 counter/log/budget)+ RequirementIds/ToolExecutionIds + 10 个 inherent helpers
  + fork/named + trace_node + 分配日志 + exhausted/with_budget 失败模式;prelude re-export SeqIds/RequirementAllocation。
- 命名冲突处理:inherent tool_call_id() 与 trait 同名共存,测试用 UFCS。
- 全套验证绿(fmt/clippy -Dwarnings/test -p/test --all 0 failed/doc -Dwarnings/diff --check)。
- 已标 TODO.md M1-2 [DONE] + 完成记录。PLAN.md 无需改(拓扑/阶段未变)。
