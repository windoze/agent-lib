# 当前任务：M2-2 resume 时用持久化高水位播种 decoder seq（M-EXT-1）

## 任务要求（TODO.md M2-2）

- decoder（或 session）提供以指定 `next_seq` 起始的构造方式；adapter resume 时用
  `ExternalSessionRef.last_event_seq`（或等价持久化字段）播种。
- 三个 adapter（claude_code / codex / opencode）行为一致；补注释说明 seq 单调性依赖。
- 若 ACP 路径有同类问题一并修。
- 验证：machine 层单元测试——"已消费到 seq=50 → resume → 新事件"场景，resume 后
  第一个 observation 不被 `observe()` 丢弃（参考 `src/agent/external/machine/tests.rs`）。
- external feature 测试与 clippy 全过。
- 设计要求见 `docs/managed-external-agent.md` §5.5（"seq spans the whole session"）。

## 执行计划

1. 探查现状：
   - 三个 adapter 的 resume 路径与 decoder/session 构造（claude_code/adapter.rs:645-651 附近，
     `ClaudeCodeSession::new` → `next_seq = 0`；codex/opencode 同构）。
   - decoder 的 seq 产生点（各 `decoder.rs` 的 `next_seq` / seq 分配逻辑）。
   - `ExternalSessionRef.last_event_seq` 字段定义与持久化位置（registry / session ref）。
   - machine.rs:732-741 `observe()` 的 `observed.seq > consumed` 去重逻辑。
   - ACP adapter 是否有同类 resume + seq 重置问题。
   - `machine/tests.rs` 现有测试替身模式。
2. 实现：
   - 各 decoder（或 session wrapper）新增 `with_start_seq(u64)` / 构造参数，resume 时播种
     `session_ref.last_event_seq + 1`（或语义等价的高水位），注意 off-by-one。
   - 三个 adapter resume 点接线；补注释说明单调性依赖。
   - ACP 若同问题，同修法；若无 resume seq 概念则记录说明。
3. 测试：machine 层"resume 后第一个 observation 不被 observe() 丢弃"测试；可能每 adapter
   补 decoder 起始 seq 单测。
4. 文档：decoder/session 注释 + `docs/review-2026-07.md` M-EXT-1 标注；
   `docs/managed-external-agent.md` §5.5 如需澄清则同步。
5. 门禁：fmt → clippy（默认 + external features）→ test（默认 + external）→ doc。
6. 更新 TODO.md（[DONE] + 完成记录），commit，stop。

## 探查结论（要点）

- 四个 decoder（claude/codex/opencode/acp）同构：私有 `next_seq: u64` 从 0 起，`emit()` 自增；无播种 API。
- 四个 session wrapper 持 `last_event_seq: Option<u64>`，`drain_and_emit()` 逐条刷新；resume 路径（claude_code/adapter.rs:630-660、codex:715-746、opencode:737-768、acp:893-916）都只用 `session.session_id`，构造全新 session/decoder（seq=0、last_event_seq=None）。
- `ExternalSessionRef.last_event_seq: Option<u64>`（mod.rs:181-197）已贯通到 resume（registry.rs:201 传完整 ref），无需接口改动。
- machine.rs:732-742 `observe()` 过滤 `seq > consumed`，consumed 来自持久化 state.session.last_event_seq。
- ACP 同病，一并修（feature external-acp）。
- machine/tests.rs：直接驱动 sans-io machine，有 `session_ref_seq`/`sequenced`/`completed_with` fixture 与 restore 模式可参考。
- 注意「design §5.5」实际指 `docs/external-agent.md` §5.5（managed-external-agent.md §5 止于 5.4）。

## 实现方案（定稿）

1. 每个 decoder 加 `with_next_seq(u64)` builder（消费 self 返回 self），注释说明 seq 跨进程单调性依赖。
2. 每个 session 的 resume 构造点：用 `session.last_event_seq` 播种——decoder `with_next_seq(last+1 or 0)`，session 的 `last_event_seq` 初始化为持久化值（避免 `session_ref()` 水位回退）。具体形态按各 adapter 现有构造读代码后定（可能是 resume 路径改用带 resume_seq 参数的构造）。
3. 测试：
   - 每个 adapter：resume 携带 last_event_seq=50 → 喂新帧 → 第一个 observation seq == 51，session_ref 水位不回退。
   - machine 层：restore 后 last_event_seq=N 的 state + seq 从 N+1 延续的 resolution → notification 不被丢弃。
4. 文档：adapter 模块/decoder 注释、review-2026-07.md M-EXT-1 标注、external-agent.md §5.5 / managed-external-agent.md 如需澄清。

## 进度日志

- [x] 读取 TODO.md / claude_plan.md，确认首个未完成任务为 M2-2（M2-1 已 DONE 且已 commit）。
- [x] 探查完成（explore agent），根因与修法确认。
- [x] 四个 decoder 各加 `with_next_seq(u64)`（含跨进程单调性文档注释）。
- [x] 四个 session 各加 `with_resume_high_water(Option<u64>)`（播种 decoder = high_water+1、恢复 session 水位），并在四个 resume 调用点接线（claude/codex/opencode/acp）。
- [x] 测试：四个 adapter 各一条 resume seq 连续性测试（首观测 seq=51、连续、水位不回退）；machine 层 `restored_machine_dedups_against_the_persisted_high_water`（snapshot/restore 后 resume 请求带旧水位、seq 51 起的观测被保留、≤水位的重放仍被 dedup）。
  - 修正：codex/opencode/acp 的 begin prelude 已发观测（水位在 begin 后即 >50），pre-advance 断言改为「水位 ≥ 50 不回退」；claude resume begin 不读帧，保留 == Some(50) 断言。
  - 修正：machine 测试原方案在 PausedForInteraction（pending turn 活跃）时无法 snapshot，改为 Completed 后 restore 再开新 turn 走真实 resume 流程。
- [x] 文档：managed-external-agent.md §5.4 后新增 M2-2 实现注记；review-2026-07.md M-EXT-1 标注 ✅。
- [x] fmt + clippy（默认 & external features）通过；external lib 334 条测试全过；默认全量 `cargo test --all --all-targets` exit 0。
- [x] external features 全量测试 48 个目标全 ok；rustdoc 门禁通过。
- [x] TODO.md 标记 [DONE] + 完成记录；review 文档 M-EXT-1 已标注。
