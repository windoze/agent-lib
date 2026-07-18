# M4-3 执行计划：为 external capability 增加来源模型

## 任务（TODO.md M4-3）
在 `src/facade/external.rs` 为 `ExternalAgentCapabilities` 增加 capability **来源**模型：
- 新增 `CapabilitySource` 枚举，至少覆盖 `Declared` / `Supplied` / `Probed` / `Negotiated`。
- `ExternalAgentCapabilities` 提供 `source()` accessor。
- 更新 builder 的 capability 校验和错误信息（`UnsupportedExternalMode`），让调用方看出判断来自哪个 source。
- 保持现有调用可编译；若改构造函数，提供兼容 helper / 迁移路径。
- serde / Debug / Clone / PartialEq 行为符合现有测试期望。

## 设计决策
1. 新增 `CapabilitySource`（Declared/Supplied/Probed/Negotiated），
   derive `Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize`，
   `#[serde(rename_all="snake_case")]`，`as_str()` + `Display`，`Default = Declared`。
2. `ExternalAgentCapabilities` 加 `source: CapabilitySource` 字段，`#[serde(default)]`。
3. 构造函数（provenance-tagged）：
   - `declared(inner)` → Declared（`for_runtime`、`from_restored_parts`、ACP preset baseline 用）。
   - `supplied(inner)` → Supplied。
   - `probed(inner)` → Probed（供 M4-4 使用）。
   - `from_runtime_capabilities(inner)` 保留，语义 = 调用方提供 = **Supplied**（通用 pub wrapper）。
   - `from_acp_negotiation(negotiated)` → **Negotiated**（cfg external-acp）。
   - `from_acp_config` baseline 改为 `declared(capabilities_from_initialize(&none()))` → Declared。
4. `source()` const accessor。
5. `.capabilities(caps)` 保留：存储调用方 caps 的 provenance；M4-4 可传 Probed。
6. `UnsupportedExternalMode` 增加 `source: &'static str`，`#[error]` 文案带 source；
   `build()` 填 `self.capabilities.source().as_str()`。

## 测试
- preset(codex) source == Declared；手工 `.capabilities(supplied(..))` == Supplied；
  `.acp_negotiated(..)` == Negotiated；`from_runtime_capabilities` == Supplied；
  错误带 source 字段。Probed 需本机 CLI，离线无法单测，靠 M4-4 + feature clippy。

## 验证命令
```
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test -p agent-lib --lib facade::external
cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

## 进度
- [ ] 实现 CapabilitySource + 字段 + 构造函数 + source()
- [ ] 更新 UnsupportedExternalMode + build()
- [ ] 更新内部构造点
- [ ] 更新/新增测试
- [ ] 验证全绿
- [ ] TODO.md [DONE] + 提交

## 状态：完成
- [x] 实现 CapabilitySource + 字段 + 构造函数 + source()
- [x] 更新 UnsupportedExternalMode(capability_source) + build()
- [x] 更新内部构造点(for_runtime/from_restored_parts/from_acp_config)
- [x] 更新/新增测试（6 新增 + 2 更新）
- [x] 验证全绿（fmt/clippy default+features/test lib/doc/full suite）
- [x] TODO.md [DONE] + 完成记录
