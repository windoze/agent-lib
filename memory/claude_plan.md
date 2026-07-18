# 当前任务计划

## 任务：M1-4 `ClientError::Network` 中 URL query 脱敏（H-SEC-4）——已完成

### 执行结果
- `src/adapter/http.rs` 新增共享 `map_transport_error`：timeout → `Timeout`；其余取 `error.url()`，query 整体替换为 `[REDACTED]`（保留 fragment）后在 Display 消息中替换原 URL；无 URL 回退原文。`read_error_body_bounded` 的 chunk 错误同样改走该 helper（同类修复）。
- 4 处本地 `map_transport_error` 副本删除，调用点统一 `http::map_transport_error`。
- `EndpointConfig.query_params` rustdoc 补充"禁止放 secret；脱敏是错误输出缓解而非凭据保护"。
- 新增 4 条测试（redact 纯函数 ×2、真实 connect 错误脱敏 ×2），全部通过。
- 全量门禁（fmt / clippy 默认+external features / test --all --all-targets / doc）全部通过。
- `docs/review-2026-07.md` H-SEC-4 标注 ✅ 已修复（M1-4）。
- TODO.md M1-4 标记 [DONE] + 完成记录。
- 无 breaking change。

### 下一步
- 提交 commit `[M1-4] ...`，然后停止（每个 invocation 只完成一个任务）。
