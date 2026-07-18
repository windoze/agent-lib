# M6-2 执行计划：全量验证默认构建、测试、文档和 external feature clippy

## 任务性质
验收/验证任务（TODO.md M6-2）。按 cheap→expensive 顺序运行完整验证命令，
确认默认构建、clippy、test、rustdoc、以及 external feature clippy 全部通过。
若失败，回到对应 milestone 修正（不只记录失败）。ignored real e2e 不强制运行，
但确认未配置时干净跳过（保持 #[ignore]）。

## 验证命令（全部必须通过）
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings
3. cargo test --all --all-targets
4. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
5. cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings

## 执行顺序（先便宜后昂贵，避免 fmt/clippy 修改后重跑 test）
- [x] 1. cargo fmt --all（无代码改动）
- [x] 2. default clippy -D warnings（EXIT=0）
- [x] 3. external feature clippy -D warnings（EXIT=0）
- [x] 4. rustdoc -D warnings（EXIT=0）
- [x] 5. cargo test --all --all-targets（EXIT=0，0 failed，10 ignored）

## 完成后
- TODO.md 标记 M6-2 [DONE]，完成记录写明各命令结果。
- 提交，停止（不进入 M6-3）。

## 进度
- [x] fmt
- [x] default clippy
- [x] external clippy
- [x] rustdoc
- [x] test suite（878 lib passed，全库 0 failed，10 ignored real e2e）
- [x] TODO.md [DONE] + commit
