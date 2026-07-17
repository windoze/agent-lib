# M1-R Review：Chat facade 正确性与文档一致性检查

## 任务（TODO.md M1-R）
只做审查 + 必要小范围收敛，不引入新功能。
- 通读 `src/facade/{mod,config,ids,error,run,chat}.rs` + `prelude`，核对 `docs/facade-api.md` §3–§6 一致。
- 核对：`Chat::ask` one-shot 不保历史、`ChatSession` 多轮保历史；tool-use → `UnexpectedToolUse`；
  pending 失败默认 cancel；snapshot 不含 secret/client。
- `prelude` 只重导已存在类型；rustdoc/doctest 可编译；`FacadeError` 变体与 §16 命名一致（允许未全加）。
- 记录取舍差异（§16 Config payload、Done boxing、Usage vs TokenUsage、R7 serde）。
- 复述式对照 M1 已实现 vs §5–§6 承诺项，缺口记为后续任务。

## 审查结论（对照 docs/facade-api.md）
- §3 模块/prelude：facade + prelude 均在；prelude 仅重导已存在类型
  （Chat/ChatSession/ModelConfig/ProviderConfig/Reply/RunEvent/RunOutput/RunStream）。
  §3 示例列表是全里程碑目标集，未列 RunStream —— 小范围补 RunStream 到示例保持一致。
- §4.1 ProviderConfig：redacted Debug、无 Serialize、env/builder/custom 构造 —— 一致。
- §4.2 ModelConfig：to_model_ref / apply_to_request / max_tokens 默认 1024 —— 一致。
- §5 Chat：ask/ask_full/session；send/send_full/stream/conversation/snapshot/restore 形状与 §5.2 完全一致。
  ask 每次新建 throwaway Conversation（不保历史）；session 复用 Conversation（保历史）。tool-use → UnexpectedToolUse。
  drive_turn 出错 cancel_pending(DiscardTurn)；stream 出错 rollback —— 默认 cancel 一致。
- §5.3 内部映射：begin_turn→build_request(effective_view)→chat→start_assistant_response→finish_assistant→commit —— 一致。
- §5.4 streaming：内部 Accumulator 折叠 Response，末尾 Done 提交；转发 TextDelta + RawStream —— 一致。
- §6.1/6.2/6.3 Reply/RunOutput/RunEvent：字段/变体齐全 —— 一致。
- §16 FacadeError：已加变体 Config/Client/Conversation/UnexpectedToolUse/InvalidState 命名与 §16 一致；
  non_exhaustive，余下变体后续里程碑补。

## 已记录取舍（doc 与实现有意差异，均已在代码 rustdoc 说明）
- `FacadeError::Config(String)` vs §16 `Config(ConfigError)`：无 ConfigError 类型，用 String（名字一致，payload 简化）。
- `RunEvent::Done(Box<RunOutput>)` vs §6.3 未装箱：避免大终态变体膨胀（run.rs 已注）。
- `Reply.usage: Option<Usage>` vs §6.1 `TokenUsage`：本 crate 具体类型是 Usage（run.rs 已注）。
- `RunEvent` 不派生 serde（PLAN.md R7）：Raw* 逃生舱不作序列化承诺。

## §5–§6 承诺项 vs M1 实现对照（无缺口）
Chat: builder/ask/ask_full/session ✅｜ChatSession: send/send_full/stream/conversation/snapshot/restore ✅
IntoUserMessage: &str/String/Message/Vec<ContentBlock> ✅｜Reply(text/usage/stop_reason) ✅
RunOutput(reply/response/usage/tool_calls/delegations/artifacts/events) ✅｜RunEvent 全变体 ✅
→ §5–§6 无缺口，无需新增后续任务。

## 计划动作
1. 小范围 doc 一致性修正：facade-api.md §3 prelude 示例补 `RunStream`（doc-only，不改编译产物）。
2. 在 TODO.md M1-R 记录审查结论 + 取舍差异 + 复述对照，标 [DONE]。
3. 验证序列 1–6（review 任务全跑一遍确认绿）。
4. 提交并停止。

## 状态：DONE
- 验证序列 1–6 全绿：fmt --check ✅ | clippy ✅ | facade:: 39 passed ✅ | full suite 绿 ✅ | doc ✅ | diff --check ✅。
- 审查无 spec 偏离、§5–§6 无缺口；仅小范围 doc 修正（facade-api.md §3 prelude 补 RunStream）。
- TODO.md 已标 [DONE] M1-R 并补完成记录。
