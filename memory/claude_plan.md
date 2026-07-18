# 当前执行计划

## 任务:M1-7 [TODO] M1 review:安全与崩溃级修复收口

来源:TODO.md 第一个未完成任务(M1-1..M1-6 均已 [DONE],M2-1 起后续任务仍为 [TODO])。

### 任务性质

Review 任务,不写新功能代码。检查项:

- 逐条核对 H-SEC-1/2/3/4、H-EXT-1、H-EXT-3 已修复,`docs/review-2026-07.md` 对应条目已标注。
- 无新增 unwrap/panic;无 secret 进入 Debug/错误消息/日志的回归(grep `REDACTED` 相关测试全过)。
- 全量门禁命令通过(任务单头部):
  - `cargo fmt --all`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
  - `cargo test --all --all-targets`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- `README.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md` 如需更新已更新。

### 执行步骤

1. [ ] git status / log 确认工作区状态与上一任务(M1-6)落点。
2. [ ] 读 `docs/review-2026-07.md` 中 H-SEC-1/2/3/4、H-EXT-1、H-EXT-3 六个条目,核对均已标注 `✅ 已修复(M1-x)`。
3. [ ] 抽查每个条目的修复实现存在(代码点位 + 测试存在)。
4. [ ] 回归扫描:grep 新增 `unwrap(`/`panic!` 路径与 `REDACTED` 相关测试;确认无 secret 泄露回归。
5. [ ] 核对 README.md / managed-external-agent.md / capability-matrix.md 与 M1 改动一致性。
6. [ ] 按序跑全量门禁:fmt → clippy(默认) → clippy(external features) → cargo test --all --all-targets(超时 ≤30min,后台跑) → rustdoc。
7. [ ] TODO.md 标记 M1-7 [DONE] + 完成记录。
8. [ ] git commit,停止。

### 进展日志

- 2026-07-19:开始,读 TODO.md 确认 M1-7 为第一个未完成任务。
- 2026-07-19:**M1-7 完成**。
  - 六条审查条目(H-SEC-1~4、H-EXT-1、H-EXT-3)在 review 文档均已标注 ✅,代码点位抽查修复在场。
  - 回归扫描:M1 触及文件无新增生产路径 unwrap/panic;REDACTED 脱敏测试随全量套件通过。
  - 文档核对:managed-external-agent.md / external-agent.md 已随 M1-5/M1-6 同步;README 与 capability-matrix 不覆盖 M1 内部行为面,无需更新。
  - 门禁:fmt / clippy(默认+external features)/ cargo test --all --all-targets(exit 0, ~35s)/ rustdoc 全绿。
  - TODO.md 已标 [DONE] + 完成记录。随后 commit 并停止。
