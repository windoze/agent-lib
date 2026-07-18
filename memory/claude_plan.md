# M2-3 执行计划：Review 非流式事件一致性

## 任务
TODO.md M2-3（Review 任务）：复核 M2-1/M2-2 落地的非流式事件一致性。

## 检查范围
1. run_full / run / stream 事件语义是否清楚。
2. approval approve / deny / fallback 路径是否都有事件。
3. 文档是否明确非流式不产生 token delta。
4. 新增 recorder 是否不改变真实 handler 执行顺序（仅观察不决策）。

## 验证条件
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::agent::
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- 手工复核 docs/refine.md “非流式 RunOutput.events 缺少审批请求” 条目状态并补充。

## 执行步骤
1. [ ] 代码复核：collect_traces / weave_approval_events / RecordingInteractionHandler
   （src/facade/agent.rs）、TapInteractionHandler（stream.rs）、enriched_approval_request
   （approval.rs）、RunEvent/RunOutput（run.rs）。
2. [ ] 复核 recorder 只观察不决策、handler 优先级不变。
3. [ ] 复核文档：docs/facade-api.md §6.2 事件一致性边界、README.md、run.rs rustdoc。
4. [ ] 复核 docs/refine.md 条目状态，补充当前修复说明（若需要）。
5. [ ] 运行验证命令（fmt/clippy/test/doc）。
6. [ ] TODO.md 标记 M2-3 [DONE] 并填完成记录；提交。

## 状态：完成
M2-3 复核完成：代码符合规范，refine.md §3 补充修复状态注记，验证全绿，已标记 [DONE]。
