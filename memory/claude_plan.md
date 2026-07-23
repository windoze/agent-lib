## Execution Plan — M4-R：M4 review（接线与集成正确性核对）

TODO.md 第一个未完成任务：**M4-R [TODO]**（M1-1~M4-3 全部 [DONE]）。
这是 review 任务，**不新增功能**，仅独立核对 M4 三个实现任务（M4-1 facade 接线 / M4-2 归一化矩阵 / M4-3 真实端点测试）的正确性与完整性，最后跑全量门禁。

### 核对清单（逐条对 TODO M4-R checklist）
1. **facade 构造器错误路径** 与既有 `*_from_env` 一致；Azure 风格 `openai_from_env` 未被误改语义。
2. **归一化矩阵顺序确定性** 保持；无 env 时默认测试树全绿。
3. **真实端点测试** 全部 `#[ignore]`、无 key 泄漏、缺 env 干净跳过。
4. **M4-3 实测结论**（或未实测标注）已写入完成记录，M5-1 可直接引用。
5. **跑全量门禁命令**（含 external features 的 clippy），全部通过。

### 已读代码（核对状态）
- `src/facade/config.rs`（534 行）：✓ 已通读。
  - `openai_chat_from_env()`（:139-149）：必需 `OPENAI_CHAT_BASE_URL`（`required_env`→`FacadeError::Config`）；可选 `OPENAI_CHAT_API_KEY`（有→Bearer，无→None）。错误路径与 `anthropic_from_env`/`openai_from_env` 同款。
  - Azure 风格 `openai_from_env()`（:110-118）：**未改语义**，仍 `api-key` 头 + `api-version` query；`openai_endpoint()` 不变。
  - `openai_chat()` builder（:172-174）+ `build()` OpenAiChat arm（:302）：Bearer 直连、忽略 api_version。
- `tests/normalization/config.rs`（143 行）：✓ 已通读。三 provider 顺序确定性（Anthropic→OpenAiResponses→OpenAiChat 末尾追加）；`build_openai_chat_target()` 三 env 门禁，无 env 静默跳过；Bearer 直连；`model: String`。
- `tests/integration_openai_chat.rs`（537 行）：✓ 已通读。6 测试全 `#[tokio::test]`+`#[ignore]`；`deepseek()`/`vllm()` 缺 env 早退 skip；从不打印 key 值；90s 超时包裹。
- M4-3 实测结论：✓ TODO.md:1252-1301 已详记（4 DeepSeek 实测过 / 2 vLLM skip；§5.1 400 规则验证成立；2 个真实 spec 细节修正）。

### 待核对（下一步）
- `src/facade/config/tests.rs`：M4-1 的 4 个 config 单测在位且通过（env 隔离 `ENV_LOCK`/`EnvGuard`）。
- `src/facade/chat.rs` `client_for_provider`：OpenAiChat 分支（M1-1 落地）形状一致。
- `src/lib.rs` 协议清单：两处已加 chat/completions。
- `tests/integration_normalization.rs` 的 `#[ignore]` 文案：已补 OpenAI Chat/Completions（M4-2 连带）。

### 门禁命令（核对清单第 5 条，必须全绿）
```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
cargo test --all --all-targets                    # timeout ≤30min
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

### 与 M4-R 正交（不抢占）
- 全库安全审查（`docs/review-2026-07-23.md`，记忆 `review-2026-07-23-acp-fs-sandbox`）的 C1/H1/M1-M4 缺陷属独立审查线，**不在 openai_chat M1–M5 范围**。M3-R 已核实 H-ROB-1 经 chat/completions 路径不可达。本 review 不处置这些无关历史问题，按任务规则不抢占 TODO 顺序。

### 收尾
- TODO.md M4-R 任务下方追加 review 记录（核对结论逐条 + 门禁输出摘要 + 发现问题及处置）。
- 标题 `[TODO]` → `[DONE]`。
- commit：`[M4-R] M4 review：接线与集成正确性核对（[TODO]→[DONE]）`。
- 停。

### 进度日志
- [x] 静态核对（config.rs / config/tests.rs / normalization config / integration_openai_chat / chat.rs client_for_provider / lib.rs 协议清单 / normalization ignore 文案）全部通过。
- [x] 门禁全绿：fmt 无 diff / clippy 默认 exit0 / clippy external exit0 / test 51 ok 行 0 failed exit0（lib 1123、openai_chat 57、facade 282；integration_openai_chat 6 ignored、integration_normalization 1 ignored）/ doc exit0。
- [x] TODO.md M4-R [TODO]→[DONE] + review 完成记录（逐条 checklist + 门禁摘要 + 问题处置）。
- [ ] commit + stop。
