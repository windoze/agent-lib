# 当前执行计划

更新时间：2026-07-19

1. 读取 `TODO.md`，只按标题是否带 `[DONE]` 判断完成状态，确定第一个未完成任务。
2. 查看最新提交信息，只有在其明确提到与该任务直接相关的未完成事项时，才把它纳入当前任务或作为前置项记录到 `TODO.md`。
3. 针对第一个未完成任务读取必要上下文，避免开放式历史问题扫描。
4. 如任务可直接完成，做最小正确实现，并补充或调整相关测试与文档。
5. 按顺序运行验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、必要时 `cargo test --all --all-targets`（完整测试最长 30 分钟）。如代码未变且仅文档/TODO 变更，则复用最近一次绿色结果并在完成记录说明。
6. 若发现未排期且影响当前任务或测试失败的问题，优先修复；若必须新增前置任务，则更新 `TODO.md`、保持当前任务未完成、提交后停止。
7. 完成后在 `TODO.md` 中给当前任务标题加 `[DONE]` 并更新完成记录；仅当阶段级计划改变时才更新 `PLAN.md`。
8. 检查 `git status`、`git diff`、`git log --oneline -10`，提交本次任务相关所有改动，然后停止，不进入下一个任务。

当前状态：已识别第一个未完成任务为 `M7-5 ContentBlock 增加反序列化兜底 variant`。最新提交 `M7-4` 明确把未知 `ContentBlock` 兜底留给 M7-5，直接相关且已由任务单排期。

实施细化：

1. `ContentBlock` 增加 `Unknown { type_name, raw }`，改为手写 `Serialize`/`Deserialize`：未知 `type` 进入 `Unknown`，已知类型仍按既有规则严格校验，`Unknown` 序列化 best-effort 原样输出 `raw`。
2. 扩展 stream 事件模型与 accumulator：新增未知块 kind/delta，使流式 adapter 可把 provider 未知块折叠为 `ContentBlock::Unknown`。
3. Anthropic/OpenAI 非流式与流式 adapter 接入未知块保留；请求构造侧对 `Unknown.raw` 直接透传。
4. conversation validator 允许 assistant 顶层 `Unknown`，用户消息和 tool-result 内容仍按既有语法拒绝，并给出 `unknown` kind。
5. 补 model、adapter 非流式/流式、conversation validator 测试；更新 `lib.rs`、审查文档和 `TODO.md` 完成记录。

进度更新：代码实现、`lib.rs` 前向兼容文档、`docs/review-2026-07.md` 标注、`TODO.md` `[DONE]` 与完成记录均已完成。验证已通过：`cargo fmt --all`、默认 clippy、external feature clippy、相关定向测试、`cargo test --all --all-targets`、rustdoc。下一步检查 diff/status 并提交本任务改动，然后停止。
