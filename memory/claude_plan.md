# 执行计划

## 当前任务：M4-6 统一 reconfig 的 resolver 来源，修复默认 resolver footgun（M-ERR-3）

来源：TODO.md 首个未完成任务（M4-6，行 1186）。

### 问题

1. queue 时机器用 `self.tool_registry_resolver` 校验（`machine/default/mod.rs:327-346`），
   apply 时 driver 用 scope 自己的 resolver（`drive/reference.rs` `ReconfigRegistryHandler`）
   —— 两个可独立配置的对象可能不一致：queue 通过、apply 失败 → 已完成 turn 被销毁 +
   reconfig 留队列下轮再失败。
2. 默认 `DeclaredOnlyToolRegistryResolver` 对任何 tool set “解析成功”但 `execute` 恒
   `UnknownTool` —— 默认接线下 reconfig 全链路假成功，之后每个工具调用开始失败。

### 设计决策

- **机器是 resolver 的唯一持有方**：`DefaultAgentMachine` 保留
  `with_tool_registry_resolver`，并新增 `tool_registry_resolver()` getter。
- **`ReferenceScope` 的 reconfig resolver 只能从机器派生**：移除
  `ReferenceScope::with_tool_registry_resolver(Arc<dyn ToolRegistryResolver>)`
  （独立配置点），改为 `with_machine_tool_resolver(&DefaultAgentMachine)`（克隆机器的
  Arc，保证同一实例）。`ReferenceScope::new(client, registry)` 的默认 reconfig 行为改为
  保守失败（显式 `InvalidRegistry` 错误，而非装一个死 registry）。
- **默认 resolver 保守失败**：新增 fail-closed 的默认 resolver（`NoToolRegistryResolver`，
  `resolve_tool_set` 恒 `Err(UnknownToolSet)`），作为 `DefaultAgentMachine::new` 的默认；
  `DeclaredOnlyToolRegistryResolver` 保留为显式 opt-in（只需 advertise tools 的 loop），
  文档注明其 declared-only 语义。

### 影响面

- `src/agent/tool.rs`：新增 `NoToolRegistryResolver`；`DeclaredOnlyToolRegistryResolver`
  文档强化。
- `src/agent/machine/default/mod.rs`：默认 resolver 换成 fail-closed；新增 getter。
- `src/agent/drive/reference.rs`：`ReferenceScope` reconfig 默认 fail-closed；
  `with_tool_registry_resolver` → `with_machine_tool_resolver(&machine)`。
- 测试：
  - `src/agent/machine/default/tests/reconfig.rs`：依赖默认 resolver 放行的用例需显式接线。
  - `tests/reference_driver.rs:865`：改为机器持 resolver、scope 从机器派生。
- 文档：`docs/agent-layer.md` §4.2 同步单一来源语义；`docs/review-2026-07.md` M-ERR-3
  状态标注在 M4-7 review 任务处理（本任务只改代码+相关文档）。

### 验证

- 新增/更新单元测试：单一来源（scope 从机器派生 → queue/apply 一致）；默认接线下
  tool-set reconfig 在 queue 时即显式报错；`ReferenceScope` 默认接线 reconfig apply 显式报错。
- `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` →
  `cargo test -p agent-lib --lib agent::` → 全量 `cargo test --all --all-targets`。

## 进度日志

- [DONE] M4-6 已完成（2026-07-19）：
  - `src/agent/tool.rs`：新增 fail-closed `NoToolRegistryResolver`；`DeclaredOnly*`
    rustdoc 标注 declared-only 语义与显式 opt-in。
  - `machine/default/mod.rs`：默认 resolver → `NoToolRegistryResolver`；新增
    `tool_registry_resolver()` getter（单一来源）。
  - `drive/reference.rs`：`ReferenceScope::with_tool_registry_resolver` 移除 →
    `with_machine_tool_resolver(&DefaultAgentMachine)`；默认 reconfig resolver fail-closed。
  - `agent/mod.rs` 导出 `NoToolRegistryResolver`。
  - 测试：machine 新增默认拒收用例；reference_driver 新增 unarmed-scope 显式失败用例；
    两条 reconfig 集成测试改为机器持 resolver、scope 派生；restore/reconfig 夹具显式 opt-in。
  - 文档：`docs/agent-layer.md` §4.2 同步。
  - 门禁全绿：fmt / clippy（默认 + external features）/ `cargo test --all --all-targets` /
    全 features 测试 / rustdoc。TODO.md 已标 [DONE] 并写完成记录。
  - 下一步：M4-7 M4 review（首个未完任务）。
