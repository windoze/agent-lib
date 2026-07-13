# 执行计划

## 约束

- 使用中文记录进展。
- `TODO.md` 是任务顺序和完成状态的唯一依据。
- 只完成第一个标题未带 `[DONE]` 的任务，完成后提交并停止。
- 不做开放式历史问题扫描；只处理当前任务直接相关或测试暴露且未排期的问题。
- 若发现阻塞当前任务的未排期前置问题，只在 `TODO.md` 插入最小必要前置任务，提交后停止。
- 不在此文件记录私密逐步推理，只记录可检查的执行计划、判断摘要和进展。

## 初始步骤

1. 读取 `TODO.md`，确定第一个标题未带 `[DONE]` 的任务。
2. 查看最新提交摘要，判断是否明确提到与该任务直接相关的未完成事项。
3. 读取当前任务涉及的代码、测试和文档，确认验收条件。
4. 如任务可直接完成，按现有架构实施最小且完整的变更。
5. 运行要求的验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`。
6. 更新 `TODO.md`：在当前任务标题前加 `[DONE]`，补充完成记录和验证结果。
7. 仅在阶段级计划变化时更新 `PLAN.md`。
8. 提交所有相关变更，提交信息包含任务编号和实际完成内容。

## 当前状态

- 已创建本计划文件。
- 已读取 `TODO.md` 和最新提交摘要。
- 第一个未完成任务是 `M6-R [TODO] Milestone 6 / Conversation Core 总 Review`。
- 最新提交为 `bbb1fd0 [M6-3] Add conversation core example and docs`，未明确提出与 M6-R 直接相关的未完成问题。
- 注意：`TODO.md` 中 M6-R 之后还有 `M7-1 [TODO]`，所以本轮完成 M6-R 后不能创建项目完成 tag `endtag`；项目级 tag 需等所有任务完成。
- 已完成 M6-R 审查与验证，并将 `TODO.md` 中 M6-R 标题改为 `[DONE]`。

## M6-R 执行步骤

1. 读取 `PLAN.md`、`docs/conversation-core.md` 和相关公开文档，按 M6-R 要求建立规范核对清单。
2. 检索 `TODO`、公开可变入口、Agent/registry/DB driver 边界等高风险点，确认没有需要前置修复的阻塞问题。
3. 运行聚焦测试：Conversation persistence、projection、boundary、pending、state machine、adapter compat 与离线 example。
4. 按要求运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`、`cargo test --doc`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`。
5. 将 `TODO.md` 的 M6-R 标题改为 `[DONE]` 并补充总 Review 完成记录，记录未创建 `endtag` 的原因是仍有 M7-1 未完成。
6. 提交本轮变更并停止。

## 已完成验证

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test conversation::pending -- --nocapture`（40 passed）
- `cargo test conversation::boundary -- --nocapture`（23 passed）
- `cargo test conversation::projection -- --nocapture`（25 passed）
- `cargo test conversation::persistence -- --nocapture`（18 passed）
- `cargo test --test conversation_state_machine -- --nocapture`（3 passed）
- `cargo test --test conversation_adapter_compat -- --nocapture`（2 passed）
- `cargo run --example conversation_core`
- `cargo test conversation -- --nocapture`（147 passed）
- `perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`
- `cargo test --doc`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`

## 剩余步骤

1. 运行 `git diff --check`。
2. 查看 `git status` 与任务标题状态。
3. 提交本轮 M6-R 文档/记录变更。
4. 停止，不进入 M7-1。
