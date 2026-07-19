# 执行计划

状态：已定位当前任务。

计划：
1. 阅读 `TODO.md`，按标题是否带 `[DONE]` 判断第一个未完成任务。
2. 检查最近提交是否直接说明该任务相关的未完成问题；只处理与当前任务直接相关或阻塞当前任务的问题。
3. 阅读当前任务相关的源码、测试和文档，确认要求、依赖和验证方式。
4. 以最小正确改动实现当前任务；如果发现必须先修复的具体前置问题，则更新 `TODO.md` 并停止。
5. 按要求运行格式化、lint 和相关测试；如有未安排的失败测试，修复或在 `TODO.md` 中安排到当前任务完成前。
6. 将当前任务标题标记为 `[DONE]`，更新完成记录；仅在阶段计划变化时更新 `PLAN.md`。
7. 检查 git 状态和差异，提交本次任务相关的所有改动，然后停止，不推进下一项任务。

进度记录：
- 已创建初始执行计划。
- 已读取 `TODO.md`，第一个未完成任务为 `M8-1 [TODO] 两个 LLM adapter 收敛公共传输/解码模块（adapter 报告 M4）`。
- 当前执行范围限定为 M8-1：抽取两个 LLM adapter 的公共 SSE decoder、HTTP 传输样板、错误映射、header/URL 工具与同 crate 内重复 helper；不推进 M8-2。
- 已实现 `src/adapter/common/`：移动既有 HTTP 安全 helper，新增公共 SSE decoder、endpoint URL/header helper、JSON collision helper；Anthropic/OpenAI adapter 已改为薄接线。
- 已完成验证：`cargo fmt --all`、`cargo test -p agent-lib --lib adapter::`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。下一步补跑 external feature Clippy 与 rustdoc。
- 已完成补充验证：`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings` 与 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 均通过。
- 已更新 `docs/review-2026-07.md` 的 LLM adapter 复制代码条目，并将 `TODO.md` 的 M8-1 标记为 `[DONE]`，写入完成记录。
- 下一步：检查最终 diff / git 状态，提交 M8-1 后停止。
