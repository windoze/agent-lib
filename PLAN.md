# 实施计划：2026-07 审查收口

本计划以 [docs/review-2026-07.md](docs/review-2026-07.md) 为唯一输入，目标是把全库审查发现的问题按依赖顺序落地修复，并在过程中保持测试、文档与实现同步。

旧版计划和任务单已归档到：

- [docs/archive/2026-07-19-refine/PLAN.md](docs/archive/2026-07-19-refine/PLAN.md)
- [docs/archive/2026-07-19-refine/TODO.md](docs/archive/2026-07-19-refine/TODO.md)

审查发现按编号引用（如 H-SEC-1、M-CONV-3），定义见 [docs/review-2026-07.md](docs/review-2026-07.md)。

## 目标

1. 消除全部 🔴 高严重度问题：凭证泄露、可 panic 的 wire 数据处理、无超时挂起、子进程误杀/泄漏、状态一致性破坏（compaction 投影空洞、Agent 毒化、静默丢数据）。
2. 收口设计承诺与实现的脱节：预算、取消、pivot、审批、session policy、provider_extras——要么补实现，要么改文档明确降级，二者必须一致。
3. 修正错误与取消语义：软拒绝与硬失败分离、取消延迟有界、错误分类可靠。
4. 收敛复制代码：两个 LLM adapter 之间、三个 CLI adapter 之间的逐字重复收敛为共享模块，同一缺陷不再需要修 N 遍。
5. 每个行为变更同步更新拥有该行为的文档；默认测试保持离线可跑。

## 非目标

1. 不重写 Conversation、AgentMachine 或 external runtime 的核心架构（sans-io、committed log + pending + projection 保持不变）。
2. 不引入新的默认依赖，不改变 external-* feature 默认关闭的现状。
3. 不把 ignored real e2e 测试改成默认必跑。
4. 不改变 secret 处理的基本策略（脱敏方向只允许收紧，不允许放松）。
5. 不做审查清单之外的无关重构；低严重度项只在 M9 批量清扫，不顺手做。
6. 1.0 前的 API 稳定性不作为约束，但 breaking change 必须在任务完成记录中显式注明。

## 排序原则

1. **先小后大**：安全/崩溃级修复（M1）改动小、独立、收益高，最先落地。
2. **先下后上**：conversation（M3）→ agent 状态机（M4）→ facade（M5）→ 预算横切（M6），上层修复依赖下层语义先稳定。
3. **先行为后结构**：复制代码收敛（M8）放在行为修复之后——每个修复先用现有测试钉住行为，再移动代码，避免在未钉住行为的代码上做大重构。代价是 H-EXT 类缺陷要按现状修三遍，可接受（单点修复小且机械）。
4. **external 栈独立成里程碑**（M2、M7 部分），可以用 feature-gated 测试独立验证，不阻塞主线。

## 里程碑

### M1：安全与崩溃级修复

消除审查 🔴 安全组与 external 两个真机必踩项。全部是小型、相互独立的修复。

覆盖：H-SEC-1（Debug 泄 key）、H-SEC-2（无超时 + 错误 body 无界）、H-SEC-3（Usage 溢出 panic）、H-SEC-4（URL 泄凭据）、H-EXT-1（30s 读超时误杀）、H-EXT-3（退出码误分类）。

重点文件：

- `src/client/config.rs`、`src/client/error.rs`
- `src/adapter/anthropic/`、`src/adapter/openai_resp/`（mod.rs、request.rs、response.rs、stream/mod.rs）
- `src/model/usage.rs`、`src/stream/accumulator/mod.rs`
- `src/agent/external/{claude_code,codex,opencode}/adapter.rs`、`acp/connection.rs`、对应 `config.rs`

### M2：external 子进程生命周期正确性

修复子进程管理的边角语义：孙进程泄漏、resume 事件吞没、decoder 错误带原文、prompt 进 argv、不 reap、prelude 无界循环、worktree repo 参数错误；并决定 `ExternalSessionPolicy`/`WorktreeManager` 的接入方式。

覆盖：H-EXT-2、M-EXT-1、M-EXT-2（评估项）、M-EXT-3、M-EXT-4、M-EXT-5、M-EXT-6、M-EXT-7、M-PROM-5。

重点文件：

- `src/agent/external/{claude_code,codex,opencode}/{adapter,decoder,config}.rs`
- `src/agent/external/acp/connection.rs`、`src/agent/external/worktree.rs`、`src/agent/external/machine.rs`
- `docs/managed-external-agent.md`、`docs/capability-matrix.md`

### M3：Conversation 正确性

修复状态一致性问题，每项补回归测试。

覆盖：H-STATE-1（compaction 投影空洞）、H-STATE-2（MessageMeta 丢失）、M-CONV-1（空校验）、M-CONV-2（递归栈溢出）、M-CONV-3（insert-only 矛盾）、M-CONV-5（迟校验）、M-CONV-6（provider id 重推导）、M-CONV-7（fork 丢 projection）。

重点文件：

- `src/conversation/projection/compaction.rs`、`src/conversation/boundary/{head,fork}.rs`
- `src/conversation/persistence/{rows,snapshot}.rs`、`src/conversation/history.rs`、`src/conversation/history/index.rs`
- `src/conversation/pending/turn.rs`
- `docs/conversation-core.md`

### M4：Agent 状态机与 drive 语义

修正错误语义（软拒绝 vs 硬失败）、取消语义（延迟有界、契约一致）、协作工具断裂、pivot/trace 冲突、reconfig 静默丢失、resolver 不一致。

覆盖：H-STATE-4、H-STATE-5、H-STATE-6、M-ERR-1、M-ERR-2、M-ERR-3。

重点文件：

- `src/agent/machine/default/{mod,tools}.rs`、`src/agent/machine/nested.rs`、`src/agent/state/{cursor,queue}.rs`、`src/agent/state.rs`
- `src/agent/drive.rs`、`src/agent/drive/reference.rs`
- `src/agent/collab/tools.rs`、`src/agent/context/trace.rs`
- `docs/agent-effect-model.md`、`docs/agent-layer.md`

### M5：facade 承诺对齐

修复 facade 对外承诺的正确性：run_full 毒化、审批文档与行为相反、流式事件缺审批、字符串匹配错误分类、cancel/pivot/provider_extras 不可达、restore 校验缺失。

覆盖：H-STATE-3、M-PROM-2（cancel/pivot 部分）、M-PROM-4、M-PROM-6、M-ERR-5、M-ADP-3、M-ADP-5。

重点文件：

- `src/facade/agent.rs`、`src/facade/agent/stream.rs`、`src/facade/agent/snapshot.rs`
- `src/facade/{chat,config,run,error,approval}.rs`
- `docs/facade-api.md`

### M6：预算端到端接线

把 BudgetHandle 记账接入 drain/drive_turn 与 facade，激活 BudgetExhausted/BudgetExceeded 路径。

覆盖：M-PROM-1、M-PROM-2（budget 部分）、L-8/L-9（预算预检原子性与 dispatch 硬出口，评估后一并收口）。

重点文件：

- `src/agent/drive.rs`、`src/agent/drive/reference.rs`、`src/agent/context/budget.rs`
- `src/agent/machine/default/`、`src/facade/agent/stream.rs`、`src/facade/config.rs`
- `docs/agent-layer.md` §1.4

### M7：adapter 健壮性与协议契约

修正错误分类顺序、Usage 事件语义契约、兼容端点容错（sequence_number、空 arguments、serde default、非 JSON 行容忍），并决定 ContentBlock 未知类型的前向兼容策略。

覆盖：M-ERR-4、M-ADP-1、M-ADP-2、facade 报告 M8、adapter 报告 L1/L2/L3、external 报告 L-1。

重点文件：

- `src/client/error.rs`、`src/stream/mod.rs`
- `src/adapter/openai_resp/stream/normalizer/`、`src/adapter/anthropic/stream/`
- `src/agent/external/{claude_code,codex,opencode}/decoder.rs`
- `src/model/content.rs`

### M8：复制代码收敛

行为已被 M1–M7 修复并由测试钉住后，收敛两份大复制：LLM adapter 公共模块、CLI adapter 共享 child-process 模块。

覆盖：adapter 报告 M4、external 报告 L-12。

重点文件：

- 新增 `src/adapter/common/`（或并入 `src/client/`）
- 新增 `src/agent/external/process/`（共享 spawn/close/kill/read 模块）
- `src/adapter/{anthropic,openai_resp}/`、`src/agent/external/{claude_code,codex,opencode}/`

### M9：低严重度清扫与文档收尾

批量处理审查 🟢 项：panic/poison 策略统一、API 打磨、性能小项、文档同步；最终全面 review 并勾销审查报告。

覆盖：审查报告全部 🟢 节剩余项、M10（lib.rs 文档）、M-CONV-4（id 索引，若 M3 未做）。

## 完成定义

每个里程碑的 review 任务必须确认：

1. 该里程碑覆盖的审查条目逐条核实已修复或明确降级（降级 = 文档与实现一致地承认现状）。
2. 全部门禁通过：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

3. 拥有该行为的文档已同步（至少检查 `README.md`、`AGENTS.md`、`docs/facade-api.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md`、`docs/conversation-core.md`、`docs/agent-effect-model.md`、`docs/agent-layer.md`）。
4. `docs/review-2026-07.md` 中对应条目已标注修复状态。
