# 当前任务执行计划

> 本文件记录可审计的决策依据、执行步骤与进度，不包含模型内部隐含推理的逐字稿。

## 初始约束与决策

- `TODO.md` 是任务顺序、依赖、要求、验证和完成状态的唯一事实来源。
- 本次只处理第一个标题未带 `[DONE]` 的任务；完成并提交后立即停止。
- 在选定任务前不做开放式历史问题排查；只检查最新提交是否明确提到与当前任务直接相关的未完事项。
- 不以缩小范围、改变既定表示或任务私有特例规避缺陷。若出现阻塞当前任务的真实前置问题，将最小前置任务插入 `TODO.md`、保持当前任务未完成、提交任务结构变更后停止。
- 任何未被后续明确任务覆盖的测试失败，都必须在本次修复或在 `TODO.md` 中排到当前任务完成之前。
- 保留用户已有改动；若确认是在恢复同一任务，则按要求把当前所有未提交文件纳入最终原子提交。

## 分步执行计划

1. 首先读取 `TODO.md`，按标题定位第一个未带 `[DONE]` 的任务，摘录其依赖、交付物、测试要求和完成记录要求。
2. 检查工作树与最新提交，仅判断是否存在与该任务直接相关的未完成事项或恢复中的改动；随后阅读该任务直接引用的 `PLAN.md`、设计文档、源码和测试。
3. 建立当前行为基线，运行与任务直接相关且成本适当的检查；若发现阻塞问题，按任务政策处理，而不绕过。
4. 采用小而聚焦的补丁完整实现任务；每完成关键实现或执行路线发生变化，就更新本文件的“进度记录”和后续步骤。
5. 补充或调整测试与必要文档，覆盖任务规定的正常路径、边界条件和错误行为；检查实现是否影响同一根因下的同类场景。
6. 按顺序验证：`cargo fmt --all`，再执行 `cargo clippy --all-targets -- -D warnings`，通过后执行任务要求的测试与不超过 30 分钟的 `cargo test --all --all-targets`，最后按任务要求构建文档或运行其他验收。
7. 所有要求满足后，在 `TODO.md` 的任务标题前加 `[DONE]` 并填写实际完成记录。仅当阶段级顺序、依赖、假设或完成标准改变时才更新 `PLAN.md`。
8. 复查差异和工作树，确认没有秘密、无关改动或遗漏；更新本文件为最终状态，创建清晰的任务提交，并验证提交与工作树状态。
9. 提交后停止，不开始下一个任务。若 `TODO.md` 已全部完成，则依其规则做最终审查并创建 `endtag`。

## 进度记录

- 状态：已读取 `TODO.md`，确认首个未完成任务为 `M1-1 Conversation 模块、强类型 identity 与不可变消息 envelope`。
- 当前任务关键要求：新增聚焦的 `conversation` 模块；定义五种只接收外部值的强类型 UUID identity；新增字段私有且不可原地修改 payload 的 `ConversationMessage`；新增独立持有 system 的可 serde `ConversationConfig`；补齐 rustdoc、稳定 serde 表示和 API/round-trip 测试。
- 当前任务验证顺序：format → 严格 clippy → 聚焦测试 → 全量测试 → rustdoc → diff check。
- Git/前置检查：任务开始时除本计划文件外工作树干净；最新提交 `df220e0` 只建立本阶段计划，未声明与 M1-1 直接相关的额外未完问题。前次全量验证为 133 passed、7 ignored、0 failed。
- 已读规范：`PLAN.md`、`docs/conversation-core.md` §0/§1/§7/§10、现有 Client `Message`、crate 文档和 README。M2 已显式排期的 tool-result 状态缺口不阻塞 M1-1。
- API 决策：使用不启用 UUID 生成特性的 `uuid` 依赖；五种私有字段 newtype 统一提供 `new(Uuid)`、`parse_str`/`FromStr`、`as_uuid`、`into_uuid`、`Display`，并派生比较、哈希和透明 serde。构造 API 没有零参数、随机或时钟路径。
- 封装决策：`ConversationMessage` 只提供 `new`、按值读取 Copy id、`payload() -> &Message` 和消费式 `into_parts`；不实现可变 getter 或 deref。`ConversationConfig` 私有保存 `Option<String>`，只提供构造、只读 getter 和消费式拆分。
- 测试决策：单元测试固定 canonical UUID JSON、五类 id round-trip、外部值原样保留、message/config round-trip 与 system 分离；`compile_fail` rustdoc 固定 id 类型不可互换及 payload 不可经共享 getter 修改的编译期边界。
- 文档决策：同步 crate 根文档与 README 的当前能力说明；`PLAN.md` 的阶段级安排不变，不修改。
- 实现进度：已新增不带生成 feature 的 `uuid` 依赖，以及 `conversation/{id,config,message}.rs`；已从 crate root 导出模块并在 conversation root 重导出公共类型。
- 测试进度：已加入五类 id canonical JSON/round-trip、外部值保持、非法 UUID、config/envelope serde、消费式拆分、Client Message 无 id、system 与 role 序列分离，以及两个 `compile_fail` API 边界用例。
- 文档进度：已补齐新模块、类型和函数 rustdoc，并同步 crate 根架构说明与 README 概览/基础用法；阶段计划没有变化，因此未改 `PLAN.md`。
- 验证进度：`cargo fmt --all` 已通过；首次严格 Clippy 暴露两处新增测试缺少反序列化目标类型标注，已直接修复并重新格式化；`cargo clippy --all-targets -- -D warnings` 随后通过。
- 聚焦验证：`cargo test conversation` 通过（新增 9 passed，0 failed）；`cargo test --doc conversation` 通过（2 个 compile-fail doctest passed）。
- 全量验证：`cargo test --all --all-targets` 通过，共 142 passed、7 个需真实凭据的既有 endpoint 测试 ignored、0 failed；所有用例均远低于 1 分钟。
- 文档验证：`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过；`cargo tree -e features -i uuid` 确认仅启用 default/std/serde，没有生成、RNG 或时钟 feature。
- API/diff 审查：新类型字段均私有；未发现 `DerefMut`、`payload_mut`、UUID 生成 API 或对 Client `Message` 的 identity 回填；初次 `git diff --check` 通过。
- 任务记录：已将 `TODO.md` 的 M1-1 标为 `[DONE]` 并写入实现、测试和实际验证结果；`PLAN.md` 阶段结构未改变，未修改。
- 最终检查：完成记录写入后的 `cargo fmt --all -- --check`、`git diff --check` 均通过；标题顺序确认 M1-1 是唯一新完成项，下一项仍为 M1-2，未执行后续任务。
- 提交范围：任务开始时工作树干净，当前 Cargo 依赖/锁文件、README、TODO、本计划、crate 导出和 `src/conversation/` 全部属于 M1-1，将原子纳入一个提交。
- 最终状态：全部任务文件已通过 cached diff check，并以 `[M1-1] Add immutable conversation identity foundation` 创建原子提交；本次调用到此停止，不开始 M1-2。
