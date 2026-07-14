# 执行计划 — M3-3 实现 record / verify / update wrapper

## 选中的任务
`TODO.md` 第一个未完成任务 = **M3-3 实现 record / verify / update wrapper**(TODO.md line 672)。
前置 M3-1 / M3-2 已 `[DONE]`(HEAD=`9557f3b`),工作树 clean。

## 任务要求(TODO.md M3-3)
- `CassetteRecorder` builder,支持 `record(path)` / `verify(path)` / `update(path)`。
- wrappers:wrap llm/tool/interaction/reconfig handler,调用真实 handler 后记录或比较 entry。
- update 必须检查显式环境变量 `AGENT_TESTKIT_UPDATE_CASSETTES=1`。
- record 也显式 opt-in `AGENT_TESTKIT_RECORD_CASSETTES=1`,否则返回 skipped/ignored 风格结果。
- 写 cassette 用临时文件 + atomic rename,避免半写文件。
- 验证:update 未启用不写文件;record 经 redactor 写稳定 JSON;verify 检测 result drift;跑全套验证命令。

## 关键设计
- 新文件 `cassette/record.rs`(保持 mod.rs 聚焦);mod.rs 加 `mod record; pub use record::{...}`。
- `RecorderMode { Record, Verify, Update }`;常量 `RECORD_ENV_VAR` / `UPDATE_ENV_VAR`。
- `CassetteRecorder { path, mode, metadata, redactor: Arc<dyn Redactor>, enabled_override, state: Arc<RecorderState> }`。
  - `RecorderState { entries: Mutex<Vec<CassetteEntry>> }`,record(|index| entry) 原子按全局 dispatch 顺序追加。
  - builder:with_redactor / with_metadata / with_enabled_override(测试钩子,绕过 env gate)。
  - is_enabled():Verify 恒 true;Record/Update = override 或 env=="1"。
  - wrap_llm/tool/interaction/reconfig(&self, impl Handler) → Recording* 包装器,克隆 state+redactor。
- Recording* 包装器:先调真实 handler,再对该 family 结果 redact→归一化→按全局 index push,原样返回结果。
  - 指纹在 redact 后的 request 上计算(与 doc §5.4 "脱敏后 canonical 形状" 一致)。
- finish() -> Result<RecorderReport, RecorderError>:
  - Record/Update:未启用 → Ok(Skipped) 不写;启用 → build cassette,pretty JSON,临时文件+rename。
  - Verify:读盘 cassette,与 live 累积 entries 逐位比对 → Verified 或 Err(Drift(Vec<EntryDrift>))。
- EntryDrift { position, family, detail },分类 family/request-fingerprint/result/count 漂移。
- RecorderError { Drift, Load, Serialize, Io };RecorderReport { Wrote, Verified, Skipped }。
- 原子写:同目录 .name.tmp.pid.nanos.n → write → rename,失败清理;必要时 create_dir_all。
- prelude 追加导出。

## 验证
fmt → clippy(-D warnings)→ test -p agent-testkit record(+ 全 crate)→ test --all --all-targets → doc(-D warnings)→ git diff --check。

## 步骤
1. [x] 读 TODO/PLAN/memory + 相关类型。
2. [x] 写 cassette/record.rs(recorder + 4 wrapper + report/error/drift + atomic write + 单测)。
3. [x] mod.rs 声明并导出 record 模块;更新 mod.rs 顶部 doc。
4. [x] prelude 导出新类型。
5. [x] fmt → clippy(clean)→ record 测试(全绿)→ 全量(agent-lib 434 + testkit 75 + smoke 2,0 failed)→ doc(clean)→ diff check(clean)。
6. [x] TODO.md 标 M3-3 [DONE] + 完成记录。
7. [ ] 提交并停止(进行中)。
