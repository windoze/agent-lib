# M4-5 执行计划：Review — managed external 可用性和 capability 来源

## 任务性质
Review 任务，复核 M4-1 ~ M4-4 落地，并按需更新 docs/refine.md 两个条目状态。
不新增功能代码（除非发现真实 bug/spec 偏差 → 走 roadblock 流程新增前置任务）。

## 检查范围（TODO.md M4-5）
1. README 和 managed external docs 是否给出可工作的 handler 装配路径。
2. 默认 feature 下是否仍不拉入 CLI adapter。
3. capability source 是否覆盖 declared / supplied / probed / negotiated。
4. unsupported capability fallback 是否基于真实 capability view。
5. 错误和测试 fixture 是否不含 secret。

## 验证命令
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::external
- cargo check --examples
- cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

## 手工复核
docs/refine.md 中：
- "managed external capability 混淆声明和验证"
- "README quick start 缺少 session handler"
两条目状态，必要时补充当前修复说明。

## 进度
- [x] 代码/文档复核（M4-1~M4-4 逐项确认一致）
- [x] 运行全部验证命令（fmt/clippy/test facade::external 22pass/check examples/feature clippy/doc 全绿）
- [x] 更新 docs/refine.md §4 §5 状态=已修复 + 修复结果
- [x] 标记 TODO.md M4-5 [DONE] + 完成记录
- [x] commit（待执行）

## 结论
Review 通过：handler 装配路径可工作、默认 build 不拉 CLI adapter、capability source 四值齐全、
unsupported fallback 基于 agent 当前持有视图、错误与测试 fixture 无 secret。无 spec 偏差，无新增前置任务。
