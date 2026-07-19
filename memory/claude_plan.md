本文件记录本次调用的可审阅执行计划与进度。不会记录私有推理链路，只记录任务识别、执行步骤、验证与提交计划。

## 执行计划

1. 读取 `TODO.md`，按文件顺序识别第一个标题未以 `[DONE]` 标记的任务；同时检查最近提交信息是否明确提到与该任务直接相关的未完成事项。
2. 阅读该任务引用的相关设计文档、测试和源码，只建立完成当前任务所需的上下文，不做开放式历史问题扫查。
3. 如发现当前任务被具体前置缺陷阻塞，优先修复该缺陷；如果无法在本次任务中直接修复，则在 `TODO.md` 中插入最小必要前置任务并停止。
4. 按当前任务要求实施最小正确代码或文档变更，避免无关重构与变通实现。
5. 运行验证：先 `cargo fmt --all`，再相关 `cargo clippy`，最后按任务需要运行测试；如执行完整 Rust 测试套件，超时不超过 30 分钟。
6. 验证通过后，将当前任务标题前缀改为 `[DONE]`，更新完成记录；仅当阶段级计划发生变化时才更新 `PLAN.md`。
7. 提交本次变更，提交信息包含任务编号或清晰任务描述；提交后停止，不进入下一项任务。

## 当前进度

- 已写入初始执行计划。
- 已读取 `TODO.md` 并确认首个未完成任务为 `M5-6 restore 路径补齐 build 同级校验（M-ADP-5）`。
- 最近提交为 `M5-5`，未发现提交信息中有与 M5-6 直接相关的额外未完成事项。
- 下一步：阅读 `src/facade/agent.rs` 与 `src/facade/agent/snapshot.rs` 的 build/restore 校验路径，抽取共享校验函数并补回归测试。
- 已抽取 `build_agent_tool_declarations` 共享校验入口，并让 fresh build 与 restore build 共同使用。restore 先重建 snapshot delegates/external overrides，再校验重新注入的 runtime 工具与 restored delegation 表面是否兼容。
- 已新增 restore 回归测试，覆盖 `ask_<name>` 合成 delegation 工具与重新注入 typed tool 重名，以及 rules/dispatcher 指向未注册 delegate 的拒绝路径。
- 下一步：运行格式化、相关测试与 clippy。
- 验证结果：`cargo fmt --all` 通过；首次 `cargo clippy --all-targets -- -D warnings` 暴露新测试中 `matches!` 移动 `error` 的问题，已改为借用并重新格式化；随后 `cargo clippy --all-targets -- -D warnings` 通过。
- 验证结果：`cargo test -p agent-lib --lib facade::agent` 通过（62 条）；`cargo test --all --all-targets` 通过；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- 已更新 `docs/facade-api.md` restore 校验说明、`docs/review-2026-07.md` M-ADP-5 修复标记，并在 `TODO.md` 将 M5-6 标记为 `[DONE]`、追加完成记录。
- 下一步：检查 git status/diff/log，确认变更范围后提交。
- 提交前检查：`git status --short` 仅显示 M5-6 相关文件；`git diff --check` 无输出；最近提交为 `[M5-5] Expose facade provider extras builders`。
- 下一步：提交本次 M5-6 变更后停止。
