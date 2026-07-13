# 执行计划

## 当前约束

- 使用中文记录进展与对外说明。
- `TODO.md` 是任务顺序和完成状态的唯一权威来源。
- 本次只完成第一个标题未带 `[DONE]` 的任务，完成后更新 `TODO.md`、验证、提交并停止。
- 不做开放式历史问题清扫；只处理当前任务直接需要或测试暴露且未排期的问题。
- 不使用 workaround。若发现当前任务被缺失前置条件阻塞，应在 `TODO.md` 插入最小必要前置任务，提交后停止。

## 初始执行步骤

1. 读取 `TODO.md`，按文件顺序定位第一个标题未带 `[DONE]` 的任务。
2. 检查最新提交信息是否明确提到与该任务直接相关的未完成问题；若相关，将其纳入当前任务或作为前置任务写入 `TODO.md`。
3. 只围绕当前任务阅读必要代码、测试和设计文档，避免无边界排查。
4. 根据任务要求实施代码或文档改动；编辑前更新本文件说明将要修改的范围。
5. 按要求先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，最后运行必要测试；如需完整测试，使用不超过 30 分钟的超时。
6. 若发现未排期失败测试，修复或在 `TODO.md` 中加入最小必要前置任务，不能在失败未处理时标记当前任务完成。
7. 完成后在 `TODO.md` 中给当前任务标题加 `[DONE]` 并补充完成记录；仅当阶段计划实际改变时才更新 `PLAN.md`。
8. 查看 git 状态，提交本次相关所有未提交改动，提交信息包含当前任务编号和清晰描述。
9. 提交后停止，不继续处理下一个任务。

## 进展记录

- 已建立本计划文件。
- 已读取 `TODO.md`，第一个未完成任务是 `M5-3 [TODO] DB-neutral parent-tree row 映射`。
- 最新提交为 `[M5-2] Implement checked conversation restore`，未发现提交信息中有直接要求先处理的未完成问题。

## M5-3 执行计划

1. 阅读现有 `conversation::persistence`、snapshot/restore、projection、history 相关实现和测试，确认 snapshot data shape、restore validator 入口和当前模块拆分。
2. 设计 DB-neutral row DTO：conversation、turn、message、tool pairing、artifact、projection/span 等记录需要包含稳定 PK/FK、parent pointer、message sequence、owner/origin、schema version，并避免任何数据库驱动绑定。
3. 实现 snapshot/history 到 rows 的确定性分解，以及打乱顺序后 rows 到 snapshot 的受检重组；重组结果必须继续走 M5-2 restore validator。
4. 为 fork/export 场景提供 insert-set/row 分解能力，证明共享祖先只引用稳定 id，不重新分配 message id 或要求 UPDATE。
5. 增加聚焦测试：线性、tool、fork、projection round-trip，打乱 rows 读取顺序后 restore 等价；缺行、重复 PK、错误 FK/seq/cycle、orphan artifact rows 明确失败。
6. 更新 rustdoc/README/TODO 完成记录；若阶段计划未变，不修改 `PLAN.md`。
7. 按顺序验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、row mapping 聚焦测试、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`。
8. 提交全部相关改动后停止。

## M5-3 当前进展

- 已新增 `conversation::persistence::rows`，包含 DB-neutral row DTO、snapshot → rows、rows → snapshot 和 insert-only diff API。
- 已为 snapshot/projection 增加 crate-private 重组入口，row 层只生成 data snapshot，不构造 live Conversation。
- 已补充 row mapping 聚焦测试和 README/rustdoc。
- 已完成验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test conversation::persistence -- --nocapture`、1800 秒上限内
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、
  额外 `cargo test --doc`、`git diff --check`。
- 已将 `TODO.md` 的 M5-3 标为 `[DONE]` 并填写完成记录。
- 下一步查看 git diff/status，提交本次改动后停止。
