# M7-4 Review：Codex adapter 正确性检查

**当前执行 = TODO.md 第一个未完成任务 = M7-4**（行 2312 `### [TODO] M7-4`；
M7-3 已 `[DONE]`，M8-1 虽 `[DONE]` 但排在其后，故 M7-4 是首个未完成任务）。

## 任务性质
Review 任务（非新实现）。做什么：
1. 检查 CLI 参数顺序、sandbox/approval mode 文档与 tests。
2. 检查 feature gate、依赖、no-secret logging。
3. 检查 cleanup 与 trace。
4. 更新 `docs/capability-matrix.md` Codex 行。

验证条件：`cargo test --all --all-targets`、
`cargo test --features external-codex -p agent-lib codex_cassette`、`git diff --check`、
完成记录列出 Codex 支持/不支持能力和真实 e2e 状态。

## Review 结论（已逐项核对源码）
- CLI 参数顺序：base_exec_args 全局 -a 在 exec 前；base_resume_args 把 -s/-p 上提顶层；
  CodexTurnSpec::args 追加 prompt/message。测试齐全。✓
- sandbox/approval 映射：4 模式全覆盖，config.rs rustdoc 文档化。✓
- feature gate external-codex off by default、无新依赖、cfg-gated re-export。✓
- no-secret logging：Debug 脱敏 env、stderr=null、decoder 诊断固定串。✓
- cleanup：close grace→ForcedKill、kill_on_drop、shutdown 分类；e2e 断言 Graceful。✓
- trace：Codex 自主运行按 thread id resume，observations 不需 run/step id（刻意差异）；
  advance 尊重 ctx.is_cancelled()。✓
- 能力：streaming/resume/artifacts/usage/graceful=true；host_tools/host_subagents/
  permission_bridge=false，声明工具/Respond* 拒绝；with_probed 取交。✓
- 无缺陷：实现与测试完整、无 workaround、无 spec 偏离。review 不改代码，只更新 doc。

## 交付
- 更新 docs/capability-matrix.md：Codex 叙述后补 Codex live adapter 实报能力表，标 e2e 状态。
- TODO.md M7-4 标 [DONE] + 完成记录。

## 验证序列
1. cargo fmt --all -- --check
2. cargo clippy --all-targets -- -D warnings（feature off + --features external-codex）
3. cargo test --features external-codex -p agent-lib codex（lib）
4. cargo test --features external-codex -p agent-lib --test agent_codex_cassette
5. cargo test --all --all-targets（<=30min）
6. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features external-codex
7. git diff --check

## 进度
- [x] 读 TODO/PLAN/源码，完成 review 核对
- [x] 更新 capability-matrix.md
- [x] 跑验证序列（fmt/clippy(off+codex)/lib codex 30/cassette 7/full suite/doc/diff 全过）
- [x] TODO.md 标 DONE + 完成记录
- [ ] commit
