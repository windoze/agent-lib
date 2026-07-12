# 当前执行计划

## 工作边界

- 本次调用只处理 `TODO.md` 中按文档顺序出现的第一个标题未带 `[DONE]` 的任务。
- 不进行开放式历史缺陷扫描；仅检查最新提交是否明确提到与当前任务直接相关的未完成问题。
- 如发现阻塞当前任务的具体前置缺陷，将按规则把最小前置任务写入 `TODO.md`、保持当前任务未完成、提交后停止。
- 不记录内部隐性思维链；本文件记录可复核的事实、决策依据、执行步骤和验证结果。

## 初始步骤

1. 阅读 `TODO.md`，严格识别第一个未完成任务及其依赖、验收标准和完成记录要求。
2. 查看最新提交说明及工作区状态，判断是否存在直接关联的未完成事项或上次中断遗留；保留用户已有改动。
3. 仅阅读与当前任务有关的 `PLAN.md`、设计文档、源码和测试，确定实现边界。
4. 完整实现该任务；若计划或关键状态变化，立即更新本文件。
5. 按要求先执行 `cargo fmt --all`，再执行 `cargo clippy --all-targets -- -D warnings`，随后运行任务指定测试与不超过 30 分钟的完整测试；处理所有未被明确排期的失败。
6. 更新 `TODO.md`：仅在任务及全部验收均完成后给标题加 `[DONE]`，并填写真实完成记录。只有阶段计划发生实质变化时才更新 `PLAN.md`。
7. 检查最终差异和状态，将本次任务相关改动（若为中断续作则包括全部未提交文件）一次性提交，使用清晰的任务编号提交信息，然后停止，不处理下一任务。

## 当前状态

- 已读取 `TODO.md` 并锁定首个未完成任务：`M1-2 Closed Turn、ToolPairing 与外部元数据`。
- 本次不会进入后续 `M1-3` validator/commit 实现；只建立 M1-2 要求的 immutable closed
  data shape、只读 API、serde DTO 边界和 crate-private draft/builder 边界。

## M1-2 细化计划

1. 检查最新提交说明与工作区状态，确认是否存在直接关联的未完成问题或中断续作文件。
2. 阅读 `PLAN.md` 的 Milestone 1 段、`docs/conversation-core.md` 中 Turn/identity/meta/serde
   相关规范，以及现有 `conversation` 模块和测试/API 风格。
3. 设计并分批实现：
   - 字段私有且只读的 `Turn`、`TurnMeta`、`ToolPairing`；
   - `Arc` 等共享只读 message 所有权，以及稳定、无锁的 serde data shape；
   - closed `ToolPairing` 的公共视图始终含 `result_msg: MessageId`；
   - crate-private draft/DTO，使 pending 可暂存无结果 pairing，但外部不能构造 live closed
     `Turn`；受检反序列化入口推迟到 M1-3 的统一 validator；
   - 确定性 equality、只读 getter、消费式/内部转换和完整 rustdoc。
4. 添加聚焦测试：多 message、parallel pairing、parent/meta、共享 message 稳定性、DTO/serde
   形状；用 compile-fail doctest 或 API 测试证明外部不能 raw construct、替换 message，且
   closed pairing 不存在 `None` result。
5. 依验证顺序运行 format、严格 clippy、聚焦测试、全量测试、rustdoc、diff check；任何失败
   均按任务政策修复或明确排期。
6. 完成后更新 README（若公共 API/能力说明需要）、`TODO.md` 标题与完成记录、此进度文件，
   最终复核并提交一次，然后停止。

## 规范核对后的设计决策

- 最新提交为已完成的 M1-1，未声明与 M1-2 相关的遗留问题；开始时工作区只有本文件的
  计划更新。
- `Turn` 内部用 `Arc<[ConversationMessage]>` 和 `Arc<[ToolPairing]>` 保存有序只读数据；
  公共 getter 只返回共享切片，克隆 `Turn` 时共享底层 allocation，不提供构造器、裸容器、
  mutable getter 或 replacement API。
- live `ToolPairing.result_msg` 使用非可选 `MessageId`，因此 public closed view 在类型上不能
  表达悬空配对；pending/restore 输入使用 crate-private `ToolPairingData` 的
  `Option<MessageId>`。
- `TurnMeta` 固定保存 `Usage`、调用方传入的 `Option<String>` 时间戳和来源、以及独立嵌套的
  `serde_json::Map` 扩展数据；不引入时钟依赖，扩展字段也不能 flatten 覆盖 message/Turn
  字段。
- `Turn` 本任务只实现确定性 `Serialize`。crate-private `TurnData` 负责可反序列化 DTO 与稳定
  JSON shape，可暂存 `result_msg: None`；live `Turn` 不实现 `Deserialize`，M1-3 再通过唯一
  validator 增加受检转换，避免本任务提前留下 unchecked closed 构造路径。
- 测试放入独立的 `src/conversation/turn/tests.rs`，覆盖稳定 JSON/DTO round-trip、多个消息、
  parallel pairing、parent/meta、Arc 共享、读取前后消息不变、draft dangling 表达，以及
  compile-fail API 边界。

## 执行进度

- 已新增 `conversation::turn` 并公开导出 `Turn`、`TurnMeta`、`ToolPairing`。
- 已实现 live closed types、共享只读 getter、`TurnMeta` 外部元数据、`TurnData` /
  `ToolPairingData` crate-private DTO，以及 live Turn 到 DTO 的单向序列化。
- 已增加独立聚焦测试和三个 compile-fail API 示例，覆盖 raw construction、原地替换消息、
  unchecked Turn deserialize 均不可用。
- README 与 crate docs 已同步 closed Turn 能力及受检构造边界；`PLAN.md` 的阶段级计划没有
  变化，无需修改。

## 验证进度

- `cargo fmt --all`：通过。
- `cargo clippy --all-targets -- -D warnings`：通过；文档补充后复跑仍通过。
- Turn 聚焦单测：6 passed；Turn compile-fail doctest：3 passed。
- `cargo test --all --all-targets`：145 个库单测和 3 个离线集成测试通过，7 个真实 endpoint
  测试按既有声明 ignored，0 failed。
- 首次严格 rustdoc 发现 public 模块文档链接 crate-private `TurnData`；这是文档 warning，已将
  私有类型名改为非链接代码文本；随后 format、clippy 与严格 rustdoc 均复跑通过。
- 完整 `cargo test --doc`：1 个正向 doctest 与 5 个 compile-fail doctest 通过。
- 实现差异的首次 `git diff --check` 通过；更新完成记录后的
  `cargo fmt --all -- --check` 与最终 `git diff --check` 也通过。

## 完成状态

- M1-2 的实现、测试、文档和验证要求均已满足，`TODO.md` 标题已改为 `[DONE]` 并写入完成
  记录。
- 最终差异/状态已审查；剩余步骤仅为提交全部本次改动并确认提交后工作区干净。计划使用
  `[M1-2] Add immutable closed turn data boundary`，提交后立即停止，不开始 M1-3。
