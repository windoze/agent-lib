# M4-4 执行计划：让 probed capability 成为 managed external agent 的真实能力视图

## 任务（TODO.md M4-4）
managed external agent 目前只持有 `Declared` capability view；即使 default handler
probe 已发现真实能力，agent 后续判断仍可能被 declared 基线误导。

实现要求：
1. `build_with_default_session_handler`：probe 成功后返回的 agent 必须持有 source=`Probed` 的 capability view。
2. `default_external_session_handler` 只返回 handler；新增不破坏旧 API 的 helper，返回
   (handler, probed capabilities)，或在 builder 内部完成等价逻辑。
3. `UnsupportedCapability` 判断必须基于 agent 当前持有的 capability view。
4. probe 失败仍走现有非 secret skip/error 路径。
5. 更新 `docs/capability-matrix.md`，明确 declared 与 probed 的区别。

验证：
- 测试：probed capability 缺某能力 → 请求该能力返回 UnsupportedCapability，含 capability 名 + source，无 secret。
- 测试：declared 支持但 probed 不支持时，以 probed 为准。
- `cargo test -p agent-lib --lib facade::external`
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`

## 设计决策
1. `build_default_registry` 改为返回 `(ExternalSessionRegistry, Option<ExternalRuntimeCapabilities>)`：
   CLI 各 arm 返回 `Some(probed)`；ACP arm 返回 `None`（能力经 live initialize 每会话协商，无离线 probe）；
   feature-disabled catch-all 走 Err 不变。
2. 新增 pub helper `default_external_session_handler_with_capabilities(agent)
   -> Result<(Arc<RegistryExternalSessionHandler>, Option<ExternalAgentCapabilities>), FacadeError>`：
   把 probed 包成 `ExternalAgentCapabilities::probed(..)`。
3. `default_external_session_handler` 保留旧签名，改为薄封装（丢弃 capabilities），向后兼容。
4. `build_with_default_session_handler`：用新 helper，attach handler；若 `Some(probed_view)` 则
   `agent.capabilities = probed_view`（source=Probed）；ACP(None) 保留 declared/negotiated。
5. facade 能力门禁：`ManagedExternalAgent::require_capability(cap) -> Result<(), FacadeError>`，
   基于 agent 当前持有的 `self.capabilities` 判断，缺失时返回新 FacadeError 变体
   `UnsupportedExternalCapability { runtime, capability, capability_source }`（Display 含 capability + source，无 secret）。
6. mod.rs re-export 新 helper。

## 测试（离线可跑，用 probed(..) 构造 + scripted handler 短路 probe）
- probed 视图缺 host_tools → require_capability(HostTools) == UnsupportedExternalCapability，
  msg 含 "host_tools" 与 "probed"，且不含 KEY/TOKEN。
- probed 缺 permission_bridge（declared for claude_code 支持）→ require_capability(PermissionBridge) 报错，
  证明以 probed 为准（source==Probed）。
- require_capability 支持时返回 Ok。

## 验证命令
```
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test -p agent-lib --lib facade::external
cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```
（真实 probe 折入 Probed 需本机 CLI+login，离线无法单测；靠 feature clippy 覆盖编译正确性 + 上述离线测试覆盖逻辑）

## 进度
- [ ] build_default_registry 返回 probed caps
- [ ] 新 helper default_external_session_handler_with_capabilities
- [ ] build_with_default_session_handler 折入 Probed view
- [ ] FacadeError::UnsupportedExternalCapability + require_capability
- [ ] mod.rs re-export
- [ ] 测试
- [ ] docs/capability-matrix.md 更新 declared vs probed
- [ ] 验证全绿
- [ ] TODO.md [DONE] + 提交

## 状态：完成
- [x] build_default_registry 返回 (registry, Option<probed>)；CLI arms Some、ACP None
- [x] 新 helper default_external_session_handler_with_capabilities；旧 API 薄封装保留
- [x] build_with_default_session_handler 折入 Probed view + 抽出 validate_external_mode 再校验 mode
- [x] FacadeError::UnsupportedExternalCapability + ManagedExternalAgent::require_capability
- [x] mod.rs re-export 新 helper
- [x] 3 新增测试（probed 门禁/declared 对照/helper feature-disabled fail-fast）
- [x] docs/capability-matrix.md（declared vs probed 小节）+ facade-api.md §11.2/§11.3
- [x] 验证全绿（fmt/clippy default+features/test lib default+features/doc default+features/examples/full suite）
- [x] TODO.md M4-4 [DONE] + 完成记录
