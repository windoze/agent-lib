# 执行计划：M3-5-1 rows 代次键 schema 变更（generation 字段 + to_rows 写入）

## 任务定位

- TODO.md 首个未完成任务：**M3-5-1**（M3-4 已 DONE；M3-5 拆解为 M3-5-1 ~ M3-5-4，M3-5-1 在 TODO.md 行 723 起）。
- 决策已定（M3-5，方案 b）：为会演进的三类行（conversation、lineage 关联、projection span）引入代次键 `generation: u64`，主键变为 `(原 key, generation)`；演进 = 插入新一代行而非更新。代次复用 `ConversationRecord.structural_version`。事实表保持原样。

## M3-5-1 范围（本任务）

1. `ConversationRecord`、`ConversationLineageTurnRecord`、`ProjectionSpanRecord` 各增加 `generation: u64`：
   - `ConversationRecord.generation` 恒等于 `structural_version`（to_rows 构造点直接用其填充/断言）。
   - lineage/span 行的 `generation` = 导出快照时刻的 conversation structural_version。
2. `CONVERSATION_ROW_SCHEMA_VERSION` 递增；`validate_schema_versions` 只接受新版本。
   - 旧版本行数据：显式报错 "schema 过旧，需迁移"（pre-1.0 不提供迁移路径，写入完成记录）。
   - 新字段**不加** `#[serde(default)]` 静默吞旧数据。
3. `to_rows`（`ConversationRowInsertSet::from_snapshot` 路径）填充 generation。

## 允许中间态

- TODO 明确允许本阶段 `into_snapshot`/diff 暂时沿用旧行为（M3-5-2/3 完成语义切换）；若中间态难以编译可与 M3-5-2 合并提交。优先尝试保持 M3-5-1 独立：
  - `into_snapshot`：同 conversation 只会有一代行（新导出），取唯一行即可——现有"必须恰好一行"的校验逻辑需要适配新字段但语义可保持。
  - `insert_set_against` diff key：本任务先不含 generation（M3-5-3 才加），但 key 是字符串拼接，若 key 不变则同 conversation 二次导出仍 InsertConflict（旧行为，允许）。
  - 关键：单元测试要求"旧 schema_version 的行集反序列化/into_snapshot 报明确错误"。

## 验证

- 现有 persistence 测试按新 schema 更新后全过。
- 新增单元测试：旧 schema_version 行集报明确错误。
- `cargo test -p agent-lib --lib conversation::persistence` 全过。
- fmt / clippy（默认 + external features）/ 全量测试 / cargo doc。

## 进度

- [x] 读取 TODO.md 定位任务（M3-5-1），写计划
- [x] 阅读 rows.rs 相关代码（Record 定义、to_rows、validate_schema_versions、测试夹具）
  - 关键结论：`ConversationRows` 单 conversation 行结构，M3-5-1 无需改 into_snapshot 的选取逻辑；
    但 `ConversationSnapshot::from_parts` 当前透传 `conversation.schema_version`，row schema
     bump 到 2 后必须改传 `CONVERSATION_SNAPSHOT_SCHEMA_VERSION`（两 schema 独立演进）。
  - 新增 `validate_generations()`：单代次行集的完整性校验（conversation.generation ==
    structural_version；lineage/span 行 generation == conversation 行代次），M3-5-2 多代次
    重组是不同输入形状的扩展，不冲突。
  - 构造点只有 rows.rs 内部三处 + `from_span`；integration 测试仅 serde round-trip，兼容。
- [x] 实现 schema 变更（三 Record 加 generation；ROW_SCHEMA_VERSION=2 与 snapshot schema 解耦；
      from_snapshot 填充；into_snapshot 加 validate_generations + 改传 SNAPSHOT_SCHEMA_VERSION；
      validate_schema_versions 报「no migration path pre-1.0」）
- [x] 新增 3 条测试（generation 填充、旧 schema 拒绝含旧 JSON 反序列化失败、代次不一致拒绝）；
      persistence 26 条全过
- [x] 全量验证全过：fmt、clippy（默认 + external features）、persistence 26 条、
      全量测试 exit 0（50 目标）、external features 测试 exit 0（48 目标）、cargo doc
- [x] TODO.md M3-5-1 标 [DONE] + 完成记录（review 文档 M-CONV-3 归 M3-5-4 后统一标注）
- [ ] 提交并停止

## 结果

M3-5-1 完成：三类演进行新增 `generation: u64`（无 serde default，旧 JSON 失败关闭）；
ROW_SCHEMA_VERSION=2 与 snapshot schema 解耦（into_snapshot 改传 SNAPSHOT_SCHEMA_VERSION）；
from_snapshot 以导出一致点 structural_version 填充；新增 validate_generations 单代次完整性
校验；validate_schema_versions 写明 pre-1.0 无迁移路径。3 条新测试，全量门禁通过。
下一步 M3-5-2：into_snapshot 按最大代次重组。
