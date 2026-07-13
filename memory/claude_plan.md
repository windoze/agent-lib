# 执行计划 — M3-2 `drain` 参考实现与 pop 路由

## 选中的任务
`TODO.md` 第一个未完成任务 = **M3-2**(M3-1 及之前全部 `[DONE]`)。里程碑 3 driver+drain 阶段 2。
非 review 实现任务,不拆分。前置 M3-1 已完成(drive.rs 有 HandlerScope + 四 handler trait)。

## 目标(TODO M3-2 "做什么")
1. `trait Pop`:向外层转交一个 requirement 并取回其 `RequirementResult`。
2. `async fn drain<M: AgentMachine>(machine, input, scope, parent: Option<&mut dyn Pop>, ctx)
   -> Result<TurnDone, AgentError>`:循环 step → 每个 requirement 查 scope handler,
   有则兑现+校验(accepts)+Resume;无则 pop 给 parent;parent=None 且无 handler →
   `AgentError::UnhandledRequirement { kind, origin }`。直至 quiescent && 无 requirement &&
   cursor ∈ {Done, Error}。
3. pop 查找从"发出者 scope 的外层"开始(跳过自身,防 §7.3 即时环)。→ 用 `ScopePop` 表示外层。
4. 决策 B:一次 step 吐一批 → 本层能兜的并发兑现(FuturesUnordered),按完成顺序 Resume。
5. 新增 `AgentError::UnhandledRequirement`(+ `AgentErrorKind::UnhandledRequirement`)于 event.rs。

## 设计
- event.rs:import `AgentPath`, `RequirementKindTag`;加 `AgentErrorKind::UnhandledRequirement`
  与 `AgentError::UnhandledRequirement { kind: RequirementKindTag, origin: AgentPath }`,扩展 kind()。
- drive.rs 新增:
  - `TurnDone { notifications, cursor }`(+accessors)。
  - `trait Pop: Send`(async fn pop(&mut self, req, ctx) -> Result<RequirementResult, AgentError>)。
  - `struct ScopePop<'a> { scope, parent }` impl Pop:resolve_requirement(scope, parent)。
  - `pub async fn drain(...)`。
  - helpers:`scope_handles`, `fulfill_with_scope`(Option), `resolve_requirement`(单个:本层→pop),
    `fulfill_batch`(本层并发 FuturesUnordered + popped 顺序), `validate`(accepts), `is_terminal`。
- mod.rs re-export:加 `drain, Pop, ScopePop, TurnDone`。

## 测试(聚焦,在 drive.rs #[cfg(test)],加在 M3-1 5 个之上)
- 本层有 handler → 兑现不冒泡(drain BatchMachine + WrappedScope,parent=None → Done)。
- 本层无 → pop 到 parent 兑现(inner EmptyScope + ScopePop(outer WrappedScope))。
- 顶层无 → UnhandledRequirement(EmptyScope, parent=None)。
- §7.3 skip-self:两层 scope,popped req 走 ScopePop(outer) 命中 outer handler,inner 同类
  handler 计数为 0。
- 一批并发乱序:多 NeedTool,handler yield 反序完成,按 id 路由结果一致,机器 Done。

## 验证命令(顺序)
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --lib agent::drive`(聚焦)
4. `cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
6. `git diff --check`

## 进度
- [ ] event.rs 加 UnhandledRequirement
- [ ] drive.rs 加 Pop/ScopePop/drain/TurnDone/helpers + docs
- [ ] mod.rs re-export
- [ ] 测试
- [ ] 全套验证
- [ ] TODO.md 标 [DONE] + 完成记录,提交

## 执行结果(已完成)
M3-2 完成:drive.rs 新增 `Pop`/`ScopePop`/`drain`/`TurnDone` + helpers;event.rs 新增
`AgentError::UnhandledRequirement`(+ Kind);mod.rs re-export。
- drain:喂 External→循环 step,本层批用 FuturesUnordered 并发兑现(完成顺序 Resume,决策 B),
  兜不了顺序 pop 给 parent(ScopePop 表示外层,§7.3 skip-self),accepts 校验,直至 cursor Done/Error。
- 顶层无 handler → UnhandledRequirement。新增 5 聚焦测试(合计 10 全过)。
- 全套验证全绿:fmt/clippy 干净;lib 419 + integration 8 = 427 passed/0 failed;doc -D warnings 干净;
  git diff --check 干净。已标 TODO.md [DONE] + 完成记录。
