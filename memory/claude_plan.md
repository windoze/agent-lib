# M1-2 Reply / RunOutput / UsageSummary / RunEvent / IntoUserMessage

**当前任务 = TODO.md 首个未完成 = M1-2**（`### [TODO] M1-2`）。M1-1 已 `[DONE]`。
唯一设计输入：`docs/facade-api.md` §5.2、§6。装配层，不新增 effect family。

## 目标（TODO.md M1-2「做什么」）

1. 新建 `src/facade/run.rs`：
   - `Reply { text: String, usage: Option<Usage>, stop_reason: Option<StopReason> }`
     + `text()/usage()/stop_reason()`；`text()` 聚合 `Response` 的 Text blocks。
     （spec 写 `TokenUsage` 但代码无此类型；TODO 注「确认实际类型名」→ 用 `model::usage::Usage`。）
   - `RunOutput { reply, response: Option<Response>, usage: UsageSummary, tool_calls: Vec<ToolTrace>,
     delegations: Vec<DelegationTrace>, artifacts: Vec<ArtifactRef>, events: Vec<RunEvent> }`。
   - `UsageSummary { supervisor, subagents, external: Usage }` + `from_supervisor/total/add_*`（M1 只填 supervisor）。
   - `RunEvent`（枚举，全变体现在就定死避免后续破坏）：TextDelta/ToolStarted/ToolFinished/
     ApprovalRequested/DelegationStarted/DelegationProgress/DelegationMessage/DelegationArtifact/
     DelegationFinished/DelegationFailed/Escalated/Done(RunOutput)/RawStream(StreamEvent)/RawNotification(Notification)。
     M1 只有 TextDelta/Done/RawStream/RawNotification 有实义，其余占位。
2. 最小占位类型（放 run.rs）：`ToolTrace/ApprovalRequest/DelegationTrace/DelegationProgress/
   DelegationMessage/ArtifactRef/EscalationTrace`，`#[non_exhaustive]`，rustdoc 注明后续 milestone 填充。
   M1 里 RunOutput 对应 Vec 默认空。
3. `IntoUserMessage` trait + 4 impl：`&str`/`String`/`Message`/`Vec<ContentBlock>` → user `Message`。
4. R7：RunEvent 归一化变体尽量可序列化；Raw* 标注非序列化承诺 → RunEvent 不 derive Serde；
   叶子数据类型（Reply/UsageSummary/traces）可 derive Serialize+Deserialize。
5. 全部公开项带 rustdoc。
6. mod.rs 重导 Reply/RunOutput/UsageSummary/RunEvent/IntoUserMessage + trace 占位类型；
   prelude 补 Reply/RunOutput/RunEvent（§3 列表）。

## 已核实代码锚点

- `client::Response { message: Message, usage: Usage, stop_reason: Normalized<StopReason>, extra }`（src/client/response.rs，`crate::client::Response`）。
- `model::usage::Usage`（derive Serialize + 自定义 Deserialize；有 `merge`、`total_computed`；Clone/Debug/Default/PartialEq/Eq）。
- `model::normalized::{Normalized<T>{value,raw}, StopReason}`（StopReason: Copy/Eq/Serialize/Deserialize）。
- `model::message::{Message{role,content}, Role}`；`model::content::ContentBlock::Text{text,extra}`（enum tag=type）。
- `stream::StreamEvent`（`crate::stream::StreamEvent`，Serialize/Deserialize/Clone/Debug/PartialEq/Eq）。
- `agent::Notification`（`crate::agent::Notification`，同上派生）。
- 无 `TokenUsage` 类型 → 用 `Usage`（TODO 已授权确认实际类型名）。

## 验证（TODO.md M1-2）

- 单测：Reply::text() 多 text block 聚合正确、非文本 content 不丢（保留在 RunOutput.response）；
  IntoUserMessage 四种输入产等价 Message；UsageSummary 聚合求和正确。
- 聚焦：`cargo test -p agent-lib facade::run`。
- 完整序列 1(fmt --check)、3(clippy -D warnings)、4(cargo test --all --all-targets)、
  5(RUSTDOCFLAGS=-D warnings doc)、6(git diff --check)。步骤 2 用聚焦名。

## 执行步骤

1. [x] 读 TODO M1-2 + 锚点类型 + facade-api §3/§5.2/§6 + PLAN R7。
2. [x] 写 src/facade/run.rs（类型 + `From` 构造器 + IntoUserMessage + 占位类型 + rustdoc）+ run/tests.rs。
3. [x] mod.rs / prelude.rs 补重导。
4. [x] fmt ✅ / clippy -D warnings ✅（Done 改 Box<RunOutput> 消 large_enum_variant）/ 聚焦 5 passed ✅ /
       cargo test --all --all-targets 50 组 ok ✅ / doc ✅ / git diff --check ✅。
5. [x] TODO.md 标 M1-2 [DONE] + 完成记录（含 TokenUsage→Usage、Box 两处 spec 取舍留给 M1-R）。
6. [~] commit `[M1-2] ...`，停（进行中）。
