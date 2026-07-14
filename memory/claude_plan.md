# 执行计划 — M3-2 实现 cassette replay handlers

## 选中的任务
`TODO.md` 第一个未完成任务 = **M3-2 实现 cassette replay handlers**(line 615)。
前置 M3-1 已 `[DONE]`(HEAD=`220a8ad`),工作树 clean。

## 任务要求(TODO.md M3-2)
- `CassetteLlmHandler: LlmHandler`
- `CassetteToolHandler: ToolHandler`
- `CassetteInteractionHandler: InteractionHandler`
- `CassetteReconfigHandler: ReconfigHandler`
- 每个 handler 按 **family + 顺序 + request fingerprint** 匹配 entry。
- mismatch 错误含 cassette path/label、entry index、family、expected fp、actual fp、请求摘要。
- 与 scripted handler 共享 call log 风格(复用 `LlmCallLog`/`ToolCallLog`/… 与 `CallLog`)。
- replay 不调用真实 handler(replay handler 本身即终端,无 delegate)。

## 关键设计决策
- 模块化:`cassette.rs` → `cassette/mod.rs`(schema 保持不变) + 新增 `cassette/replay.rs`;mod.rs `mod replay; pub use replay::*;`。honors M3-1 doc "replay 扩展 crate::cassette"。
- `CassettePlayer`:持 `Arc<Cassette>` + label,`.llm_handler()/.tool_handler()/.interaction_handler()/.reconfig_handler()` 各造独立 handler。
- 每 handler 持:该 family 的 `Vec<*Entry>`(构造时按 family 过滤克隆)+ `label: Arc<str>` + `cursor: Mutex<usize>` + `Arc<CallLog>`。
- 匹配:算 `request_fingerprint(request)`;取 cursor 处 entry;None→exhausted;fp 不等→mismatch;相等→advance+返回记录 result。
- mismatch 折叠(遵循现有 "family-aligned failure" 哲学):
  - LLM → `Llm(Err(ClientError::Other(msg)))`
  - Tool → `Tool(Err(ToolRuntimeError::ExecutionFailed{tool_name, message}))`
  - Reconfig → `Reconfig(Err(ToolRuntimeError::InvalidRegistry{message}))`
  - Interaction → 无 Err 家族 → **panic**(loud,与 ScriptedInteractionHandler panic 一致)。
- `ReplayMismatch`(pub)+ `ReplayMismatchKind`(Fingerprint/Exhausted);Display 含全部字段;accessors 便于测试。
- outcome→RequirementResult:replay.rs 内私有 helper(不改 M3-1 schema)。
- prelude 追加导出 handler + player + mismatch 类型。

## 验证
fmt → clippy(-D warnings)→ test -p agent-testkit(replay + 全 crate)→ test --all --all-targets → doc(-D warnings)→ git diff --check。

## 步骤
1. [x] 读 TODO/PLAN/memory + 相关类型。
2. [ ] git mv cassette.rs → cassette/mod.rs;加 replay 模块声明与导出。
3. [ ] 实现 replay.rs(handlers + player + mismatch + 单测)。
4. [ ] prelude 导出。
5. [ ] fmt → clippy → 聚焦测试 → 全量 → doc → diff check。
6. [ ] TODO.md 标 M3-2 [DONE] + 完成记录。
7. [ ] 提交并停止。

## 完成状态(2026-07-14)
- [x] git mv cassette.rs → cassette/mod.rs;replay 模块声明与 pub use 导出。
- [x] 实现 replay.rs(4 handler + CassettePlayer + ReplayMismatch/Kind + 10 单测)。
- [x] prelude 导出。
- [x] fmt(clean)→ clippy -D warnings(clean)→ replay 测试(10 passed)→ 全量(agent-lib 434 + testkit 68 + smoke 2,0 failed)→ doc -D warnings(clean)→ diff check(clean)。
- [x] TODO.md 标 M3-2 [DONE] + 完成记录。
- [ ] 提交并停止(进行中)。
