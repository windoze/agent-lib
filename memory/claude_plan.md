# M1-4 Review — 协议层完整性与兼容性检查

**当前执行 = TODO.md 第一个未完成任务 = M1-4**(M1-1/M1-2/M1-3 已 `[DONE]` 且已 commit)。
这是 Milestone 1 的阶段 review 任务(真实任务,不可跳过)。

## 任务要求(TODO.md M1-4)
1. 对照 docs/managed-external-agent.md §5,确认下列类型全部存在且有 rustdoc:
   ExternalObservedEvent / ExternalToolBatchId / ExternalToolCall / ExternalToolResult /
   ExternalSubagentRequestId / external subagent request+output DTO /
   ExternalSessionInput::{RespondToolResults,RespondSubagent} /
   ExternalSessionResult::{PausedForToolCalls,PausedForSubagent}。
2. 检查 src/agent/external/mod.rs 的 pub use / 公开路径是否符合 crate 风格。
3. 检查新增 DTO 是否保留 raw/extra escape hatch,且不把 runtime 私有 schema 泄露为稳定 typed API。
4. 检查 docs/managed-external-agent.md 命名,如实现采用了不同命名则同步更新文档。
5. 验证:external_dto_roundtrips / requirement /
   RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace / 完整验证序列 1-6。
6. 完成记录列出 M1 public API diff 摘要。

## 核查结论(阅读源码后)
- 全部 8 类类型均存在于 src/agent/external/mod.rs,均带完整 rustdoc(含 design §5.x 引用)。
- src/agent/mod.rs re-export 完整、按字母序、风格一致(external::{...} 一块 + sink 一块)。
- raw escape hatch:ExternalToolCall/ExternalToolResult/ExternalSubagentRequest/ExternalSubagentOutput
  均有 raw: Option<Value>(#[serde(default, skip_serializing_if)]);to_tool_call 主动丢弃 raw,
  不泄露到稳定 tool 路径;raw 类型是 serde_json::Value,不暴露 runtime 私有 typed schema。OK

## 需要动手的唯一工作:文档命名同步(M1-3 memory 显式把此项 defer 到 M1-4)
实现相对 docs §5 的偏差:
- §5.1 RespondSubagent.output: 文档 SubagentOutput -> 实现 ExternalSubagentOutput。
- §5.2 PausedForSubagent: 文档平铺 request_id/spec_ref/brief/result_schema ->
  实现嵌套 request: ExternalSubagentRequest;并更新 "推荐 spawn_agent tool call" 取舍说明为已定的
  专门变体方案(spawn_agent tool bridge 留给 M3-3)。
- §5.3 ExternalToolResult: 补 error: Option<String> 字段并对齐字段顺序;补
  ExternalSubagentRequest / ExternalSubagentOutput 结构定义。
- §21 M1 line: "决定 spawn_agent 走 tool bridge 还是专门 PausedForSubagent" -> 记录已选专门变体。

## 步骤
- [ ] 编辑 docs/managed-external-agent.md §5.1 / §5.2 / §5.3 / §21 命名同步
- [ ] cargo test -p agent-lib external_dto_roundtrips
- [ ] cargo test -p agent-lib requirement
- [ ] cargo fmt --all -- --check(仅 doc 改动,代码未变)
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- [ ] cargo test --all --all-targets(仅 md 改动,复用上次绿;跑聚焦即可)
- [ ] git diff --check
- [ ] TODO.md 标 [DONE] + 完成记录(含 M1 public API diff 摘要)
- [ ] commit,stop

## 备注
本任务只改文档(*.md)。代码未变 -> 完整 test suite 可复用 M1-3 的绿结果;仍跑聚焦
external_dto_roundtrips + requirement + doc + clippy 以满足验证条件与确保 rustdoc intra-doc link 无破损。

## 完成状态(2026-07-17)
全部步骤完成:核查通过;docs §5.1/§5.2/§5.3/§21 命名已同步实现;
external_dto_roundtrips(1)+requirement(40) 绿;clippy 0;rustdoc -D warnings 绿;
全量 suite 复用 M1-3 绿(仅 md 改动);git diff --check clean。TODO.md 已标 [DONE] + 完成记录。待 commit。
