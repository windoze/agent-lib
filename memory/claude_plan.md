# 执行计划（2026-07-19）

## 当前任务：M2-4 [TODO] Codex/OpenCode prompt 传参加固（M-EXT-4）

### 任务理解（来自 TODO.md）

问题：
- `src/agent/external/codex/adapter.rs:121-124`：`args.push(prompt.clone())` 作为最后位置参数，
  前面没有 `--` 分隔符；`opencode/adapter.rs:127-143` 同样。以 `-` 开头的用户消息会被 clap
  当成 flag（例如用户输入以 `--model` 开头会导致 CLI 解析失败/语义改变）。
- prompt 明文出现在 argv 中，对本机 `ps` 可见（信息泄露面）。Claude Code 走 stdin frame 无此问题。

要求：
1. 在 prompt 前插入 `"--"` 分隔符；先用 `codex exec --help` / `opencode --help` 或官方文档确认
   两个 CLI 支持 `--`（记录确认结果）。若某 CLI 不支持，改为其支持的等价机制（如 stdin）并在
   代码注释说明。
2. `ps` 可见性：评估 stdin 传 prompt 的可行性；若维持 argv，在
   `docs/managed-external-agent.md` 安全节明确记载该暴露面与理由。

验证：
- 单元测试：构造以 `--model` 开头的 prompt，断言生成的 argv 含 `--` 分隔且 prompt 原样位于其后。
- external feature 测试与 clippy 全过；`#[ignore]` real e2e 手工抽查一次（如环境允许）。

### 执行步骤

1. 阅读 `codex/adapter.rs` 与 `opencode/adapter.rs` 的 argv 构造点（约 121-124 / 127-143 行附近），
   以及 `claude_code` 的 stdin 传 prompt 方式作对照。
2. 检查本机是否有 `codex` / `opencode` CLI（`which codex opencode`，`codex exec --help`），
   确认 `--` 分隔符支持；若无本机二进制，查官方文档/web。
3. 在两个 adapter 的 argv 构造中于 prompt 前插入 `"--"`，补注释说明理由。
4. 评估 stdin 传 prompt 可行性（codex `exec` 支持从 stdin 读 prompt 吗？opencode `run` 呢？），
   记录选型；若维持 argv，更新 `docs/managed-external-agent.md` 安全节记录 ps 暴露面。
5. 新增单元测试：argv 断言（`--model` 开头 prompt → argv 含 `--` 且 prompt 紧随其后）。
6. 找出现有 argv 构造相关测试（如有），更新断言。
7. `docs/review-2026-07.md` 的 M-EXT-4 条目打标 `✅ 已修复（M2-4）`。
8. 运行门禁：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` →
   `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings` →
   `cargo test --all --all-targets` → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
9. TODO.md 把 M2-4 标题改 `[DONE]`，追加完成记录（含 CLI `--` 确认结果、ps 暴露面决策）。
10. commit（`[M2-4] ...`），停止。

### 进展日志

- [x] 步骤 1-2：阅读代码 + 确认 CLI `--` 支持（codex clap 原生；opencode yargs `populate--: true` 源码确认）
- [x] 步骤 3-5：实现 + 测试（两 adapter `args()` 插 `--`；更新 2 条既有断言 + 新增 2 条 `--model` 前缀测试）
- [x] 步骤 6-7：文档（managed-external-agent.md §16 新增暴露面节 + §13/§14 同步；review doc 打标；AGENTS.md 条目）
- [x] 步骤 8：门禁全过（fmt / clippy 默认+external / 全量测试 / external 测试 / doc）
- [x] real e2e 抽查：codex（16s）与 opencode（10s）均通过
- [x] 步骤 9-10：TODO 完成记录 + commit

任务 M2-4 已完成。
