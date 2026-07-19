当前任务执行计划

说明：本文件记录可检查的执行计划和进度摘要，不包含私有推理。

1. 读取 `TODO.md`，按文件顺序找出第一个标题未以 `[DONE]` 开头的任务。
2. 查看该任务的要求、依赖、验证方式和完成记录；必要时查看最新提交是否直接提到与该任务相关的未完成事项。
3. 只围绕该任务收集代码上下文，避免无关历史问题排查。
4. 实现该任务；如果遇到阻塞该任务的真实前置问题，在 `TODO.md` 中插入最小必要前置任务并停止。
5. 按要求运行格式化、lint 和相关测试；若发现未安排的失败测试，修复或将其明确排入 `TODO.md`。
6. 更新 `TODO.md`：任务完成时在标题前加 `[DONE]`，并填写完成记录；仅当阶段级计划变化时更新 `PLAN.md`。
7. 检查工作区差异，提交本次任务相关变更，然后停止，不继续下一个任务。

进度：已识别第一个未完成任务为 `M6-2 [TODO] facade budget 旋钮 + dispatch 预算硬出口`。

当前任务执行步骤：

1. 核对最新提交是否包含与 M6-2 直接相关的未完成前置；结果：最新提交为 `[M6-1] Wire budget accounting into drain`，与当前任务直接衔接，无需新增前置。
2. 阅读 facade 构建/run 路径、`RunContext`/`BudgetLimits` 接线，以及 external dispatch 的预算选择逻辑。
3. 增加 facade builder 的 `budget(BudgetLimits)` 配置入口，并确保每次 run/stream 使用配置的预算限制创建 `RunContext`。
4. 将预算耗尽终态映射为 facade 用户可识别的结构化错误/事件，避免只暴露普通 agent error。
5. 在 external dispatch 可用预算为 0 时返回显式预算耗尽结果，不再派 cheapest worker。
6. 补充单元测试与文档；随后执行 `cargo fmt`、clippy、相关测试、全量测试和 rustdoc。

进度更新：已完成代码初稿。

- `AgentBuilder` / `AgentRestoreBuilder` 增加 `budget(BudgetLimits)`，`Agent` 持有 per-run budget 并用于普通、rules-routed、dispatcher-routed、streaming run context。
- facade 将 `LoopDoneReason::BudgetExhausted` 映射为 `FacadeError::BudgetExhausted`。
- `agent::external::Dispatcher` 在预算已耗尽或 evaluator step charge 超限时返回 `DispatchError::BudgetExhausted`，不再降级派 worker。
- 已补 facade budget 测试、dispatch 硬出口测试，并同步 facade / agent-layer / external-agent / review 文档。

验证结果：全部通过。

- `cargo fmt --all`
- `cargo test -p agent-lib --lib facade::`（226 条通过）
- `cargo test -p agent-lib --lib agent::external::dispatch`（16 条通过）
- `cargo clippy --all-targets -- -D warnings`
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
- `cargo test --all --all-targets`（默认离线全量通过，真实端点/CLI 测试保持 ignored）
- `cargo test --features "external-claude-code external-codex external-opencode external-acp" --all-targets`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`

进度更新：已将 `TODO.md` 的 M6-2 标题标记为 `[DONE]` 并填写完成记录。下一步检查工作区差异并提交本次任务。

最终检查：`git diff --check` 通过；修正文档注释后已重跑 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。准备提交。
