# M6-3 执行计划：Review 最终正确性和完整性验收

## 任务性质
最终验收/复核任务（TODO.md M6-3）。这是项目最后一个任务；完成后需打 `endtag` tag。

## 检查范围
1. 所有 milestone review 任务是否完成（M1-3、M2-3、M3-5、M4-5、M5-3）。
2. PLAN.md / TODO.md / docs/refine.md 是否一致。
3. README quick start 是否能让新调用方避开已知坑。
4. 默认测试、feature clippy、rustdoc 是否都通过。
5. 是否还有必须本轮修复但未排入任务的设计目标缺口。

## 验证条件
- `rg "\[TODO\]" TODO.md` 完成后不应再有未完成任务标记（仅剩 legend 说明行）。
- `git diff --check` 通过。
- 完成记录列出：修复的设计目标差距、仍保留的非阻断风险、已运行的验证命令。

## 验证命令
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings
3. cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
4. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
5. cargo test --all --all-targets

## 进度
- [x] 读取 TODO/PLAN/refine，确认一致
- [x] 审阅 README quick start（build_with_default_session_handler，避开 handler 坑）
- [x] 运行全量验证（fmt/clippy x2/rustdoc/test 全 EXIT=0）
- [x] TODO.md 标记 M6-3 [DONE] + 完成记录
- [ ] commit + 打 endtag tag
