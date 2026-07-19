# 执行计划：M3-4 消除长链递归（restore 校验 + History drop）（M-CONV-2）

## 任务定位

- TODO.md 首个未完成任务：**M3-4**（M3-1/2/3 已 DONE，M3-4 行 679 起）。
- 三处递归：
  1. `src/conversation/persistence/snapshot.rs` `visit_parent`：restore 环检测，递归深度 = 父链长。
  2. `src/conversation/history.rs` `build_restored_node`：restore 重建节点，同样递归。
  3. `RawEntry` cons 链表与 `HistoryNode.parent` 链递归 drop——长会话析构 `History` 栈溢出。

## 关键探索结论

- `Turn` 无公开构造器，唯一认证入口是 `validation::validate_turn_data`（`pub(super)`，conversation 子孙模块可用）。
- **额外发现**：除递归外还有两处 O(N²) 长链问题——
  1. `validate_parent_graph` 的连通性检查对每个 turn 调 `root_of` 沿父链走到根（O(chain) per turn）；
  2. `validate_raw_turns` 的跨 turn 身份校验每 turn 扫全部 retained——commit 路径同构，属既有设计成本模型。
  - 决策：`root_of` 备忘录化（同函数内、行为完全保持、同属长链缺陷类，且 100k 验证测试需要）；`validate_raw_turns` 的 O(N²) 身份校验**不改**（commit 共享验证器，规则漂移风险，超出 M-CONV-2 递归范围），完成记录如实说明。因此 100k 测试直测 `validate_parent_graph` + `History::from_restored` + drop，而非全量 `Conversation::restore`。
- `HistoryNode`/`RawEntry` 无 by-value 移动字段的使用点，加 `impl Drop` 安全。
- 测试落点：新建 `src/conversation/persistence/snapshot/tests.rs`（可同时访问私有 `validate_parent_graph`/`raw_turn_index` 与 `pub(crate)` 的 `History::from_restored`）。

## 实施内容

1. `visit_parent` → `check_parent_chain`：迭代链走 + 显式 path 标记，ParentCycle 报错语义逐位保持。
2. `root_of`：按 index 备忘根，连通性检查 O(N²) → O(N)，错误顺序/字段不变。
3. `build_restored_node`：显式栈上攀到已建祖先再反向构建。
4. `HistoryNode::drop` / `RawEntry::drop`：循环 `Arc::try_unwrap` 摘链，共享引用即停。
5. 4 条 100_000 链测试（合计 < 1s）：长链校验通过、全链环报错、from_restored + drop、共享链 drop 后 fork 完好。

## 进度

- [x] 读取 TODO.md 定位任务（M3-4），写计划
- [x] 阅读代码，确认改动面与测试方案
- [x] `visit_parent` 迭代化 + `root_of` 备忘录化（snapshot.rs）
- [x] `build_restored_node` 迭代化 + 两个手工 Drop（history.rs）
- [x] 10 万级链测试（persistence/snapshot/tests.rs，4 条）
- [x] 验证全过：fmt、clippy（默认 + external features）、conversation:: 162 条（0.63s）、
      全量 `cargo test --all --all-targets`（exit 0，无 FAILED）、cargo doc
- [x] `docs/review-2026-07.md` M-CONV-2 标注 ✅；TODO.md M3-4 标 [DONE] + 完成记录
- [ ] external features 测试确认 + 提交
