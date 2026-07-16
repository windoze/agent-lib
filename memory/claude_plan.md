# M5-3 — artifact ref 记录(patch / diff / test result)

**状态:完成(全绿,已提交)。**

## 目标(TODO.md M5-3)
- 落实 `ExternalArtifactRef` 字段(已在 M2-1 定义:kind/summary/path/reference)。
- machine 完成时把 `ExternalAgentOutput.artifacts` 记录到 state(=可持久化 trace),
  只记引用与摘要,不落敏感原文(§12 redaction)。
- 提供把 `FilePatch` event 归集为 artifact 的映射 helper。

## 关键事实(已核对)
- DTO 已就绪:`ExternalArtifactKind {Patch,Diff,TestResult,File,Other}`、
  `ExternalArtifactRef {kind,summary,path,reference}`、`ExternalAgentOutput.artifacts`(mod.rs)。
- `ExternalAgentState`(state.rs)字段:spec/conversation/session/cursor/active_tools/cleanup_required。
  序列化经 `ExternalAgentStateRecord`(deny_unknown_fields)。
- machine `complete_session`(machine.rs:463)消费 `output`:仅用 summary/usage 建 response,artifacts 被丢弃。
- Failed 无 output,故 artifacts 仅在 Completed 记录。
- 无独立 trace 对象;state 是可持久化 trace,notification 流是实时 trace。

## 方案
1. state.rs:`ExternalAgentState` 加 `artifacts: Vec<ExternalArtifactRef>`。
   - 访问器 `artifacts(&self) -> &[ExternalArtifactRef]`。
   - `record_artifacts(&mut self, IntoIterator<Item=ExternalArtifactRef>)` 追加。
   - Record 加字段 `#[serde(default, skip_serializing_if = "Vec::is_empty")]` 保持向后兼容。
   - new() 初始化空 vec。
2. mod.rs:
   - `impl ExternalArtifactRef { pub fn from_file_patch(&ExternalAgentEvent) -> Option<Self> }`
     (FilePatch → kind=Patch, path/summary/diff_ref→reference;非 FilePatch 返回 None)。
   - 自由函数 `collect_file_patch_artifacts(&[ExternalAgentEvent]) -> Vec<ExternalArtifactRef>`,再导出。
3. machine.rs `complete_session`:在建 response 后、settle 前
   `self.state.record_artifacts(output.artifacts)`(move,只记引用与摘要)。
   更新 rustdoc 说明记录 artifacts。
4. 测试:
   - `external_agent_records_artifacts`(machine/tests.rs):Completed 带 artifacts →
     `direct.state().artifacts()` 记录正确 ref,且结构上不含原文;空 artifacts 时不记录。
   - state.rs 单测:artifacts round-trip / 空时不出现在快照。
   - mod.rs 单测:`from_file_patch` / `collect_file_patch_artifacts` 映射。
5. 验证序列:fmt --check → `cargo test external_agent_records_artifacts` → clippy -D warnings →
   全量 test → doc -D warnings → git diff --check。
6. TODO.md M5-3 标 [DONE] + 完成记录;提交 `[M5-3] ...`,停止。

## 约束
- 不改既有 record 字段/顺序的 wire 兼容(新增字段可跳过)。
- machine 保持 sans-io;不记录原文,只记 ref/summary。
- 新公开 API 带 rustdoc。
