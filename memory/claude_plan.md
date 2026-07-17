# M5-4 Review：runtime abstraction 与离线 e2e 完整性检查

**当前执行 = TODO.md 第一个未完成任务 = M5-4**（M1..M4、M5-1、M5-2、M5-3 均 `[DONE]`）。

## 任务理解
M5-4 是**审查任务**（review），是真实 adapter（M6-M8）前的边界冻结点。
目的是确认后续 Claude/Codex/OpenCode 只需在 adapter 层填 parser + process 管理，
不需要改 machine/driver。这是真实任务，不能跳过。

## 目标（TODO.md 做什么）
1. 检查 `ExternalSessionHandler` 生产路径是否只组合 registry + adapter，不含 machine 状态逻辑。
2. 检查 scripted tests 是否覆盖：tool / interaction / subagent / mixed tool+subagent /
   observations live sink + buffered replay / cancel cleanup。
3. 检查 cassette schema 是否文档化并脱敏。
4. 更新 `docs/managed-external-agent.md` runtime adapter 章节的实现状态。

## 验证条件
- `cargo test -p agent-lib scripted_external`
- `cargo test -p agent-lib external_cassette`
- 完整验证序列 1-6 全过。
- 完成记录中列出真实 adapter 必须实现的 trait 方法和错误映射。

## 执行计划
1. [进行中] 读 runtime.rs（ExternalRuntimeAdapter/Session/Registry + Handler 生产路径）。
2. 读 scripted tests + cassette 测试，逐项核对 6 类覆盖；缺口需补测试。
3. 读 cassette.rs 与 fixtures，确认 schema 文档化 + redaction。
4. 更新 docs/managed-external-agent.md runtime adapter 章节实现状态。
5. 验证序列 1-6。
6. TODO.md 标 [DONE] + 完成记录（含真实 adapter 必须实现的 trait 方法 + 错误映射清单）。
7. 提交 `[M5-4] ...` 并停止。

## 状态：调查中

## 状态：完成
- [x] 核对 handler = registry+adapter，无 machine 状态（runtime.rs/cassette.rs/registry.rs）
- [x] 覆盖核对：tool/interaction/subagent/live-sink/buffered-replay/cancel-cleanup 已覆盖
- [x] 补缺口测试 scripted_external_mixed_tool_and_subagent_round_trip（唯一缺口）
- [x] 核对 cassette schema 文档化 + 脱敏（scan_secrets/assert_no_secrets/fixtures_are_redacted）
- [x] 更新 docs/managed-external-agent.md（§1 能力表 + §11.4 实现状态 + trait/错误映射）
- [x] 验证序列 1-6 全过（fmt / scripted 5 / cassette 9 / clippy / 全量 40 binary 0 failed / doc / diff-check）
- [x] TODO.md 标 [DONE] + 完成记录（含真实 adapter trait 方法 + 错误映射清单）
- [ ] 提交 `[M5-4] ...`（本轮最后一步）
