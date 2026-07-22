## Execution Plan — M3-R：M3 流式正确性核对

TODO.md 第一个未完成任务：**M3-R [TODO] M3 review：流式正确性核对**。M3-1/M3-2/M3-3
均已 [DONE]。这是一个**只读 review 任务**（不新增功能），逐条核对 M3 流式实现与设计文档
§4.4 的一致性，跑全量门禁，写 review 记录。

### 范围与边界（关键）

- `docs/review-2026-07-23.md` + memory 的 C1/H1/M1-M4 是**独立的全库安全审查**，涉及 ACP
  fs 沙箱、accumulator `apply_unknown_delta` panic、read_line DoS、conversation 状态一致性
  等，**不在 openai_chat 适配器 M1–M5 任务线上**。按任务规则，不相关历史问题不抢占当前
  TODO 顺序；baseline 全绿（无失败测试触发 Test Failure Policy）。故这些**不阻塞 M3-R**，
  本任务不扩大范围去修它们。
  - 唯一交叉点：M3-R checklist 含「Accumulator 折叠对照测试存在且通过」。H-ROB-1 是共享
    accumulator 对 unknown block 的潜在 panic（需特定恶意输入触发），与 openai_chat 流式
    正确性正交；本任务只确认折叠对照测试通过，不在此修 H-ROB-1。

### 核对清单（来自 M3-R 任务正文）

1. 设计文档 §4.4 四个关键差异逐条对照实现：
   - 哨兵特判在 JSON 解析前；
   - `index` 键控增量不中途解析 JSON；
   - reasoning 落点正确（BlockKind::Reasoning + Delta::Reasoning，无 signature）；
   - 终态双源（finish_reason + usage chunk）无重复 MessageStop。
2. 与 M2 一致性：finish_reason 映射表两处共用同一份代码（无复制粘贴漂移）；
   Accumulator 折叠对照测试确实存在且通过。
3. fixtures 脱敏检查：无真实 key、token、账号、内网地址。
4. 状态机对乱序/缺失字段的健壮性：缺 id 的后续 chunk、空 delta、未知字段不 panic。
5. 跑全量门禁命令（fmt / clippy 默认+external / test --all / doc），全部通过。

产出：在 M3-R 任务下方追加 review 记录（核对结论 + 门禁摘要 + 发现的问题及处置）。

### 执行步骤

1. [ ] 读设计文档 §4.4（docs/openai-chat-api.md）+ §7.1 折叠对照要求。
2. [ ] 读 M3 实现：stream/{wire.rs,decoder.rs,normalizer.rs,mod.rs} +
       stream/tests/{mod.rs,parsing.rs,errors.rs,transport.rs,fixtures/*} +
       response/convert.rs（finish_reason 映射，确认流式与响应侧共用）。
3. [ ] 逐条核对 4 个关键差异 + M2 一致性 + fixtures 脱敏 + 健壮性。
4. [ ] 跑全量门禁（fmt / clippy 默认 / clippy external / test -p openai_chat /
       test --all / doc）。
5. [ ] 在 TODO.md M3-R 追加 review 记录 + [TODO]→[DONE]。
6. [ ] commit + stop。

### 进度日志

- [x] 上下文读取（TODO §M3-R + 设计文档 §4.4/§7.1 + M3 实现 wire/decoder/normalizer/mod +
      tests/{mod,parsing,errors,transport} + 8 fixtures + response/convert + common/sse + 全库 review docs）
- [x] 逐条核对 §4.4 四个关键差异 + M2 一致性（finish_reason 单一映射 + 折叠对照）+ fixtures 脱敏 + 健壮性
      （哨兵前置 / index 键控零解析 / reasoning 落点 / 双源无重复 MessageStop / normalizer 零 panic）
- [x] 门禁全绿：fmt 无 diff / clippy 默认 PASS / clippy external PASS / test -p openai_chat 57 通过 /
      test --all 全 0 failed（lib 1119）/ doc PASS
- [x] 脱敏 grep：fixtures 仅 demo 值无 IP；`sk-`/`Bearer` 命中均在测试代码（脱敏断言/假占位密钥/错误 body 文案）
- [x] TODO M3-R [TODO]→[DONE] + review 记录（5 条 checklist 逐条 + 门禁摘要 + 范围外观察：H-ROB-1 accumulator
      panic 经 chat/completions 路径不可达，属独立全库审查线，不阻断 M3-R）
- [ ] commit + stop
