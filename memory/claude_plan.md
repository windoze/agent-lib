# 执行计划 — M3-1 `HandlerScope` 与四个 handler trait

## 选中的任务
`TODO.md` 第一个未完成任务是 **M3-1**(M2-R 及之前全部 `[DONE]`)。里程碑 3 driver+drain 阶段 2。
非 review 实现任务,不拆分。

## 目标(TODO M3-1 "做什么")
1. 新建 `src/agent/drive.rs`,从 `agent/mod.rs` 导出(`pub mod drive;` + 公共 re-export)。
2. `trait HandlerScope`(Send+Sync):四个默认返回 `None` 的方法
   `llm()/tool()/interaction()/subagent()`,分别返回
   `Option<&dyn LlmHandler/ToolHandler/InteractionHandler/SubagentHandler>`。
3. 四个 handler trait(`#[async_trait]`,Send+Sync):
   - `LlmHandler::fulfill(&self, request: &ChatRequest, mode: LlmStepMode, ctx: &RunContext) -> RequirementResult`
   - `ToolHandler::fulfill(&self, call_id: ToolCallId, call: &ToolCall, ctx: &RunContext) -> RequirementResult`
   - `InteractionHandler::fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult`
   - `SubagentHandler::fulfill(&self, spec_ref, brief, result_schema, ctx) -> RequirementResult`
     (阶段 0 仅定义签名;实现留 M5,doc 标注)。
4. 契约:handler 返回的 `RequirementResult` 变体必须与请求 kind 对齐(drain 用 M1-1
   `RequirementKind::accepts` 校验)。M3-1 在 doc 写明,并在测试断言。

## 范围边界(不做)
- `drain` / `Pop` / `UnhandledRequirement` → M3-2。
- 参考 driver(真正包装 client/registry/policy 的公共类型 + 复跑 50 集成测试)→ M3-3。
- 本任务只交付 trait 定义 + `#[cfg(test)]` 最小 fixture。

## 测试(聚焦)
- `EmptyScope: HandlerScope {}` → 四个默认方法全 `None`。
- `WrappedScope` 挂 llm/tool/interaction(不挂 subagent)→ 前三 `Some`、subagent `None`。
- fixture 把 `LlmClient`/`ToolRegistry`/`ToolApprovalPolicy` 包装成 handler;
  各自 `fulfill` 结果用对应 `RequirementKind::accepts` 断言通过。

## 验证命令(顺序)
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --lib agent::drive`(聚焦)
4. `cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
6. `git diff --check`

## 进度
- [ ] 写 drive.rs(trait 定义 + docs)
- [ ] mod.rs 导出
- [ ] 测试 fixture + 断言
- [ ] 全套验证
- [ ] TODO.md 标 [DONE] + 完成记录,提交

## 执行结果(已完成)
M3-1 完成:`src/agent/drive.rs` 新增 `HandlerScope` + 四个 handler trait,mod.rs 导出。
- HandlerScope 四访问器默认 None;四 handler trait(async_trait,Send+Sync)签名与 TODO 一致;
  SubagentHandler 仅签名(实现留 M5)。返回路径类型对齐由 M1-1 accepts 校验(drain 阶段 M3-2)。
- 5 个聚焦测试(EmptyScope None、WrappedScope Some/subagent None、三 handler accepts 对齐)。
- 验证全绿:fmt / clippy -D warnings / drive(5) / full(414 lib +8 integ=422) / doc / diff --check。
M3-1 已标 [DONE] 并写完成记录。下一次调用处理 M3-2。
