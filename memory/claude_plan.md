# M3-3 执行计划：让 restore 优先使用 snapshot 中的协作内容

## 任务（TODO.md M3-3）
更新 `AgentRestoreBuilder::build` / `CollabState::restore`，实现冲突策略：
**snapshot 内容为准，topology 只作为兼容旧 snapshot 的 provision hint**。
- snapshot 有 mailbox/blackboard/plan 内容 → 优先从 snapshot 恢复（即使 topology 未启用）。
- snapshot 缺内容但 topology 要求启用 → 才建空组件。
- snapshot 内容与 topology 冲突 → snapshot 权威；恢复后 `config` 需拓宽以保持一致。
- 恢复后 agent 能继续执行协作工具 / delegate workflow。
- 写入文档（docs/facade-api.md §15.2）。

## 现状调研（M3-2 已落地）
- `src/facade/collab.rs` `CollabState::restore(config, ids, mailbox, blackboard, plan)`：
  **当前是 topology 权威**——`config.*_enabled()` 决定是否建，snapshot 只补内容。
  冲突时（config 未启用但 snapshot 有内容）会丢弃 snapshot 内容 → 需要改成 snapshot 权威。
- `CollabState.config` 被 `Agent::collaboration()`（agent.rs:455）、Debug（:203）读取；
  `mailbox()/blackboard()/plan()` 访问器和 `CollabBridge::from_state` 读取 Option 原语。
  → 若 snapshot 恢复了 topology 未声明的原语，必须同步拓宽 `config`，避免
  `collaboration()` 与 `mailbox()` 不一致。
- `Collaboration` 有 const builder `.mailbox()/.blackboard()/.plan()` 可拓宽 flag。
- `AgentRestoreBuilder::build`（snapshot.rs:793）用 `resolve(None, delegation, ...)` 从
  topology 派生 config，再传 snapshot slices 给 `CollabState::restore`。

## 实现步骤
1. `src/facade/collab.rs` 改写 `CollabState::restore`：
   - 每个原语：`snapshot.map(from_snapshot).or_else(|| config.*_enabled().then(空建))`。
   - 用恢复结果拓宽 effective `config`（原语 is_some → 置对应 flag）。
   - 更新 doc 注释，说明 snapshot 权威 / topology 作为旧 snapshot provision hint。
2. `docs/facade-api.md` §15.2 增加协作 restore 冲突策略说明段落。
3. 更新 snapshot.rs `build()` 注释（原“substrate flags 决定 which”改为 snapshot 权威）。
4. 测试（`facade::agent::snapshot::tests`，snapshot_tests.rs 追加）：
   - 冲突：base agent（无 delegate，config 空）用手工带 mailbox 内容的 snapshot restore →
     mailbox 恢复且 `collaboration().mailbox_enabled()` 为真、内容可读、续 seq。
   - 旧格式兼容：用不含 mailbox/blackboard/plan/artifacts 字段的 JSON 反序列化为
     AgentSnapshot（`#[serde(default)]`），restore 成功、得到空协作底座。
   - （round-trip seq / blackboard offset 续操作 M3-2 已覆盖，确认保留。）

## 验证
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::agent::snapshot
- cargo test -p agent-lib --lib facade::collab
- cargo test -p agent-lib --lib agent::collab（回归）
- cargo test --all --all-targets
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

## 状态：完成
M3-3 完成：`CollabState::restore` 改为 snapshot 权威（topology 仅作 provision hint），恢复后
拓宽 config 保持一致；facade-api.md §15.2 写入冲突策略与旧格式兼容说明；新增 4 个测试
（2 facade::agent::snapshot 冲突/旧格式，2 facade::collab restore 语义）。fmt/clippy/targeted/
full/doc 全绿，TODO.md 标记 [DONE]。
