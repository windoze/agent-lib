# 执行计划 — M3-1 定义 cassette schema、redactor 与 fingerprint

## 选中的任务
`TODO.md` 第一个未完成任务 = **M3-1 定义 cassette schema、redactor 与 fingerprint**(line 559)。
M1-* / M2-* 全 `[DONE]` 且已提交(HEAD=`cd1fdc1` = M2-R),工作树 clean。前置依赖 M2-R 已满足。

## 任务要求(TODO.md M3-1)
在 `crates/agent-testkit/src/cassette.rs`(当前仅 skeleton stub)实现:
1. `Cassette`:schema version、metadata、entries、optional observations。
2. entry 类型:`LlmEntry`/`ToolEntry`/`InteractionEntry`/`ReconfigEntry`(统一 tagged enum `CassetteEntry`)。
3. request fingerprint:canonical JSON(排序 key)作为 v1 fingerprint 字符串。
4. fingerprint 默认忽略 volatile ids:RequirementId、TraceNodeId、测试分配 id、tool_call id/step_id 等。
5. `Redactor` trait + `DefaultRedactor`:默认保留 message 文本,redacts provider extras 未知字段。
6. schema version 常量;未知版本 deserialize 分类失败。

只做 M3-1;不实现 replay handlers(M3-2)。

## 关键设计决策
- payload 均可序列化;`ToolRuntimeError` 不可序列化 → testkit 内定义镜像 `CassetteToolError` + 双向 From,
  不改 agent-lib(与 `ClientError` 可序列化先例一致)。
- outcome:LlmOutcome/ToolOutcome/InteractionResponse/ReconfigOutcome。
- `CassetteEntry` internally-tagged(tag="family"),M3-1 不含 Subagent(AgentError 属 M5)。
- fingerprint:to_value → 递归 canonicalize(排序 key + volatile-id key 字符串值→`<volatile-id>`)→ compact JSON。
  `input`/`input_schema` 子树 opaque:排序但不 strip id。
- `Cassette::from_json_str` 先读 schema_version 分类(missing/unsupported/ok)再 full parse。
- `DefaultRedactor` scrub `provider_extras.fields` 与 `response.extra` 非 allowlist 值→`<redacted>`;message 文本不动。

## 验证:fmt → clippy(-D warnings)→ test -p agent-testkit → test --all --all-targets → doc → diff check。

## 步骤
1. [x] 读 TODO/PLAN/memory + agent-lib 相关类型。
2. [x] 实现 cassette.rs + prelude 导出。
3. [x] fmt → clippy(clean)→ 聚焦(11 passed)→ 全量(绿)→ doc(clean)→ diff check(clean)。
4. [x] TODO.md 标 M3-1 [DONE] + 完成记录。
5. [ ] 提交并停止(进行中)。
