## Execution Plan — M5-R：M5 review + 最终收口归档

TODO.md 第一个未完成任务：**M5-R [TODO]**（M1-1~M5-2 全部 [DONE]）。这是整个 openai_chat
适配器计划的**最后一个任务**——最终 review + 收口结论 + 归档。

### 任务范围（逐条对 TODO M5-R 核对清单）
1. **§8 文档同步清单逐条销号**：DESIGN.md / capability-matrix.md / README.md / AGENTS.md /
   client-layer-references.md 五个文档同步项确认 M5-1 已完成。
2. **`DESIGN.md` 矛盾表述核对**：全文 grep `chat/completions`、`DeepSeek`、`vLLM`，确认不存在
   与本适配器矛盾的「不支持」表述。
3. **§2.1 第一期目标三条逐条验收**：① 适配器 + 两方言（DeepSeek/vLLM）；② 三层测试
   （模块单测/transport/#[ignore] 真实端点）；③ 归一化矩阵。
4. **§2.2 非目标确认未被偷渡**：无 logprobs 建模（只能进 extra）、无 n>1 多 choice、
   无 quirk 配置类型、采样参数走 extras。
5. **规模核对**：实现/测试行数与 §9 估算（实现 1200–1500 + 测试 800–1000）量级是否相符，
   超标说明原因（chat 协议简单但 fixture/折叠对照测试多）。
6. **全部任务 [DONE] 或显式降级**：确认 M1-1~M5-2 + M1-R/M2-R/M3-R/M4-R 全 DONE。
7. **最终全量门禁**（含 external features clippy）：fmt / clippy×2 / test --all --all-targets /
   doc -D warnings。
8. **PLAN.md 追加最终收口结论**（比照 `docs/archive/2026-07-20-mag-gaps/PLAN.md` 体例）。
9. **归档**：PLAN.md + TODO.md → `docs/archive/2026-07-23-openai-chat/`。

### 验证方式（不凭记忆，逐条对照代码/文档）
- §8 销号：读 5 个文档相关段落，确认与代码（capability.rs/config.rs/lib.rs/adapter/mod.rs）一致。
- grep 核对：`chat/completions` / `DeepSeek` / `vLLM` 在 DESIGN.md 的全部出现位置。
- §2.2 防偷渡：grep `logprobs` / `Quirk` / `n > 1` 在 `src/adapter/openai_chat/` 内的建模情况。
- 规模核对：`wc -l src/adapter/openai_chat/**/*.rs` 分实现 vs 测试统计。

### 执行步骤
1. 并行收集验证证据：grep DESIGN.md / 5 文档 §8 对照 / src/adapter/openai_chat 行数 / 非目标 grep。
2. 跑全量门禁（fmt→clippy×2→test→doc），记录摘要。
3. 在 PLAN.md 末尾追加「最终收口结论（M5-R）」章节（比照 mag-gaps 体例）。
4. 在 TODO.md M5-R 任务下方追加最终 review 记录（逐条对 checklist + 门禁摘要 + 规模核对）。
5. 标记 M5-R [TODO]→[DONE]。
6. 创建 `docs/archive/2026-07-23-openai-chat/`，把 PLAN.md + TODO.md 复制进去。
7. 提示是否在根目录保留 PLAN.md/TODO.md 或删除——**比照 mag-gaps 归档惯例：归档是复制还是移动**。
   （先查 mag-gaps 归档时根目录是否还留 PLAN.md/TODO.md。当前根目录有 PLAN.md/TODO.md，说明归档是复制；
   但 mag-gaps 之后又新建了当前这对——所以归档后根目录的应移走。需确认。）
8. commit（含 PLAN.md 收口结论 + TODO.md review 记录 + archive/ 新文件 + 根目录 PLAN/TODO 处置）。
9. stop。

### 进度日志
- [x] 收集验证证据：DESIGN.md grep（不支持清单只剩 Gemini，无矛盾）/ §8 五项 spot-check 全落地 /
  §2.2 防偷渡（logprobs 仅 doc+fixture+extra 未建模、无 Quirk 类型、无 n>1）/ 规模（实现 1492 在区间内、
  测试 2367 超 2.4×）/ 归档惯例确认（git mv，mag-gaps 范例 d98ca39）。
- [x] 全量门禁全绿：fmt exit0 / clippy×2 exit0 / test 51 套件 0 failed（1381 passed, 16 ignored）/
  doc exit0。
- [x] PLAN.md 追加「最终收口结论（M5-R）」；TODO.md M5-R review 记录 + [TODO]→[DONE]。
- [x] 归档：`git mv` PLAN.md/TODO.md → `docs/archive/2026-07-23-openai-chat/`（设计文档保留 docs/）。
- [x] commit + stop。整个 openai_chat 计划收口。
