# 当前任务计划：M2-3 decoder 错误消息与"不折叠原文"承诺对齐（M-EXT-3）

## 任务理解

TODO.md 第一个未完成任务是 **M2-3**。

问题：三个 external decoder 文档承诺 "Every diagnostic is a fixed string; no prompt text, tool
input, or credential is ever folded into an error message."，但实现违背：

- `src/agent/external/claude_code/decoder.rs:517-521`：把 `frame.get("result").and_then(Value::as_str)`
  原文塞进 `ExternalAgentError::Runtime.message`
- `codex/decoder.rs:446-453`：`turn.failed` 的 `error.message` 同构
- `opencode/decoder.rs:496-509`：`error.data.message` 同构

该文本受模型输出影响，可含模型读到的任意文件内容。

## 执行步骤

1. 阅读相关代码：
   - 三个 decoder 的错误构造点
   - `ExternalAgentError` 定义（enum 形状、Display、serde）
   - `machine.rs:713` 使用点（敏感原文是否会经 Display 进入 cursor/日志）
   - 错误类型的全部 match / Display / 序列化使用点
2. 选定方案（任务推荐 a）：
   - (a) `message` 固定字符串 + 原文放入单独的、标注 "may contain runtime output, do not
     log blindly" 的字段
   - 检查 `error.to_string()` 的 Display 实现不输出原文
3. 实现：
   - `ExternalAgentError::Runtime`（或相关变体）改形状：固定 message + 单独原文字段
     （注意 serde 兼容：该错误是否被持久化/cursor 化？若参与 serde，新字段
     `#[serde(default)]`）
   - 三个 decoder 改构造点
   - Display 实现不折叠原文
4. 测试：
   - 新增单元测试：构造含敏感字样（如 `API_KEY=...`）的 runtime error frame，断言
     `error.to_string()` 不含该字样
5. 文档：同步三个 decoder 模块文档承诺
6. 验证：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
   - `cargo test --all --all-targets`
   - `cargo test --features "external-claude-code external-codex external-opencode external-acp" --all-targets`
   - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
7. `docs/review-2026-07.md` M-EXT-3 标注 `✅ 已修复（M2-3）`
8. TODO.md 标记 M2-3 [DONE] + 完成记录
9. 提交 commit

## 探索结论（子代理报告）

- `ExternalAgentError` 定义在 `src/agent/external/mod.rs:989-1061`，thiserror Display，全 enum serde。
  `Runtime { code: Option<String>, message: String }`，Display `external runtime error: {message}` 直接嵌入不可信文本。
- 4 个不可信文本构造点：claude_code/decoder.rs:532-539、codex/decoder.rs:463-477、
  opencode/decoder.rs:513-533（decode_error）、acp/adapter.rs:998-1012（classify_error）。
- 泄露链：decoder → adapter Err → ExternalSessionResult::Failed → machine.rs:713
  `error.to_string()` → `fail_with` 写入两个可持久化 cursor（ExternalAgentCursor::Error /
  LoopCursor::Error）→ facade 错误消息。修 Display 即可切断全链。
- testkit 固定字符串构造点（cassette.rs:785、runtime.rs:363、mod.rs:181、
  assertions/external.rs:213）可保留 message。
- 需要更新的断言：tests/agent_claude_code_cassette.rs:592、agent_codex_cassette.rs:482、
  agent_opencode_cassette.rs:639（原断言 message == 运行原文，改为断言新字段）；
  mod.rs:1246-1299 serde round-trip 测试加新字段。
- 现成测试范式：mod.rs:1301-1324 `unsupported_capability_display_does_not_leak_*`。

## 方案（选型 a）

- `Runtime` 增加字段 `runtime_output: Option<String>`（`#[serde(default, skip_serializing_if)]`，
  rustdoc 标注 "may contain runtime output, do not log blindly"）；`message` 改为各 runtime
  固定字符串（claude: "claude code runtime error"；codex: "codex turn failed"；opencode:
  "opencode session failed"；acp: "acp agent reported an error"）。Display 不变但 message 已固定
  → 不再泄露。旧 serde 数据缺新字段可反序列化（default → None）。
- 四个构造点把原文移入 `runtime_output`（opencode 的 `error.name` 回退也进 runtime_output，
  同为不可信远端文本）。

## 进度

- [x] 读取 TODO.md，确定当前任务为 M2-3
- [x] 探索相关代码
- [x] 实现修复：`ExternalAgentError::Runtime` 新增 `runtime_output: Option<String>` 字段
  （serde default + skip_serializing_if，rustdoc 标注不可信）；5 个构造点（claude/codex/
  opencode decoder、acp decoder handle_error、acp adapter classify_error）原文移入
  `runtime_output`，`message` 改固定字符串；testkit 4 处固定诊断补 `runtime_output: None`；
  real_e2e stdout/stderr tail 移入 `runtime_output`
- [x] 测试更新：三 cassette 断言改验 runtime_output；codex/opencode cassette fixture 用
  `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 再生成；新增 5 条泄露测试（mod.rs Runtime Display
  + 三 cassette decoder + acp decoder inline，均断言含 `API_KEY=...` 的帧 error.to_string()
  不含敏感字样）
- [x] cassette 测试全过（claude 8、codex 8、opencode 8、acp 4）
- [x] fmt + 双 clippy 通过
- [x] 全量测试（默认套件 exit 0 约 49s；external features 套件无 FAILED）
- [x] doc 构建（默认 + external features 各一遍，-D warnings 通过）
- [x] TODO 完成记录 + review 标注
- [x] 提交：71cd824 [M2-3] Keep untrusted runtime error text out of ExternalAgentError Display

## 任务完成

M2-3 已落地并提交。下一任务为 M2-4（Codex/OpenCode prompt 传参加固），留待下次调用。
