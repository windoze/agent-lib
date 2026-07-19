# 当前执行计划

## 约束

- 以 `TODO.md` 为唯一任务顺序与完成状态来源。
- 本轮只完成第一个未标记 `[DONE]` 的任务，然后停止。
- 如遇到阻塞当前任务的具体前置问题，先在 `TODO.md` 中插入最小必要前置任务并提交，然后停止。
- 不做开放式历史问题泛扫；仅处理会阻塞当前任务或使当前任务行为失效的问题。
- 不记录隐藏推理链；本文件只记录可审阅的任务识别、执行计划、关键决策与进度。

## 已识别任务

- 已读取 `TODO.md`。
- M1 到 M9-1 的任务标题均已标记 `[DONE]`。
- 本轮第一个未完成任务是 `M9-2 [TODO] API 打磨批`。

## 当前任务范围：M9-2 API 打磨批

任务要求是逐项实现或显式记录“不做”及理由，并为每项提供测试或编译期保证。涉及清单：

- `ApprovalRequest::call_id` 空串哨兵改为 `Option<String>`。
- `Normalized` 构造约束与只读访问器。
- `prelude` 高频导出补齐。
- facade 配置校验缺口：空模型名、非法 temperature、空 delegate 工具名/关键词。
- `FacadeApproval.pending` 跨 run 清理。
- `ChatSessionBuilder` 支持清除继承 system prompt。
- `Some(Usage::default())` 与 `None` 语义统一。
- `FacadeIds` 全局唯一措辞修正。
- 删除 `QueuedReconfig` 兼容别名。
- 命名/文档叙事一致性修正。
- `ToolCall` 是否补 `extra` 逃生舱。
- `RunEvent` 稳定形状承诺与不产生 variant 的处理。
- `ConversationRows::insert_set_against` existing 多 conversation 输入放宽项决策。

## 执行步骤

1. 检查最近提交信息与当前工作区，确认是否存在与 M9-2 直接相关的未完成事项或未提交恢复工作。
2. 针对 M9-2 清单逐项定位代码、文档和现有测试，记录每项实现成本、breaking 面和验证点。
3. 按最小正确改动逐项落地：优先实现明确低风险 API 修复；对明显超出本批目标或不值得做的项，在 `TODO.md` 完成记录中明确“不做”理由。
4. 为每个实现项补充测试或编译期保证；文档措辞项同步更新拥有该行为的文档或 rustdoc。
5. 运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，再运行带 external features 的 clippy，最后运行 `cargo test --all --all-targets` 与 rustdoc。若只剩文档改动且已有绿色结果可复用，则按任务规则记录跳过理由。
6. 更新 `TODO.md`：将 `M9-2` 标题改为 `[DONE]`，填写完成记录、验证结果与 breaking change 汇总；仅阶段级计划变化时更新 `PLAN.md`。
7. 检查 `git status`、`git diff`、`git log --oneline -10`，提交本轮相关更改后停止。

## 当前进度

- 已定位当前任务：`M9-2 [TODO] API 打磨批`。
- 已检查最近提交与工作区：最近提交为 `[M9-1] Unify poison recovery policy`，未提示 M9-2 相关未完成事项；工作区初始仅有本计划文件变更。
- 已完成第一轮定位：M9-2 主要集中在 facade/model/agent state API、facade 文档与少量 conversation rows 决策。
- 关键落地决策：实现 `ApprovalRequest::call_id: Option<String>`、`Normalized` 私有字段与访问器、prelude/model re-export、builder 配置校验、approval pending 清理、ChatSession 清 system prompt、delegate-only usage 语义、FacadeIds 文档修正、删除 `QueuedReconfig` 别名、RunEvent non-exhaustive、`ToolCall.extra`；`ConversationRows::insert_set_against` 多 conversation existing 放宽若需要签名级重构则记录为本批不做。
- 已完成第一批实现补丁：上述实现项均已落到代码/测试/文档；`ConversationRows::insert_set_against` 放宽项暂未改代码，准备在完成记录中按签名级重构记录本批不做理由。
- 已运行 `cargo fmt --all` 与 `cargo test -p agent-lib --lib`，lib 测试 1008 条通过；期间补齐了 agent-testkit 的 `ToolCall.extra` 构造漏点和若干 `ApprovalRequest::call_id` 旧断言。
- 完整门禁已通过：`cargo fmt --all`、默认 clippy、external features clippy、`cargo test --all --all-targets`、external features 全目标测试、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- 已更新 `docs/review-2026-07.md` 的 M9-2 覆盖项状态。
- 已将 `TODO.md` 的 M9-2 标为 `[DONE]` 并写入完成记录；阶段级计划未变化，`PLAN.md` 未更新。
- 下一步：检查差异、提交本轮更改并停止。
