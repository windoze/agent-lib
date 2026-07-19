# 当前执行计划

说明：本文件记录可审计的执行计划和进度更新，不记录私有推理链。

1. 读取 `TODO.md`，按文件顺序定位第一个标题未以 `[DONE]` 标记的任务。
2. 查看最近提交信息，判断是否明确提到与该任务直接相关的未完成问题；如有，将其纳入当前任务或作为前置项写入 `TODO.md`。
3. 阅读当前任务涉及的说明、代码、测试和文档，确认验收标准与依赖。
4. 以最小正确改动实现当前任务；如遇到阻塞当前任务的规格不匹配或失败测试，先修复，或把必要前置任务插入 `TODO.md` 后停止。
5. 运行规定验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，再按需运行相关测试或完整测试套件；若只改文档且已有可复用绿色结果，则记录跳过原因。
6. 更新 `TODO.md`：将完成任务标题加 `[DONE]`，填写完成记录；仅在阶段计划确实改变时更新 `PLAN.md`。
7. 检查 Git 状态和差异，提交本次任务相关所有变更。
8. 完成一个任务后停止，不继续处理下一个任务。

当前状态：已确认最近提交未留下与 `M1-4` 直接相关的未完成事项。进一步核查发现，per-delegate external start 工具会被机器审批门豁免，`ask_tool("ask_<delegate>")` 不能按原文档作为异步工具门；这直接影响本任务的降级前提。计划改为方案 (a)：当父级注入异步 `InteractionHandler` 且 external-start 策略需要 ask 时，把启动审批构造成带 delegate/depth 归因的 approval interaction 转发给父级 handler；无父级 handler 时保持现有同步 `Approval::ask` / headless deny 行为。随后新增 allow/deny 测试，更新文档与 `TODO.md` 完成记录。

进度更新：已在 `DelegationToolHandler` 的 external-start gate 中加入父级异步 handler 路由，新增 `FacadeApproval::external_start_requires_ask` 用于区分 ask tier 与 auto allow/deny；已新增 allow/deny 离线测试，并同步 `docs/mag-gaps.md`、`docs/facade-api.md`、`docs/managed-external-agent.md`。下一步运行格式化和测试验证。

验证更新：`cargo fmt --all` 通过；`cargo test -p agent-lib --lib facade::delegate` 通过；`cargo clippy --all-targets -- -D warnings` 通过；`cargo test --all --all-targets` 通过；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 初次发现公开 rustdoc 链接到 crate-private helper，已修正后重跑通过。下一步更新 `TODO.md` 完成记录并提交。

进度更新：`TODO.md` 已将 `M1-4` 标记为 `[DONE]` 并写入完成记录。下一步检查 `git status`/`git diff`/最近提交，确认变更范围后提交。
