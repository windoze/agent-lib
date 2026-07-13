# 执行计划

## 约束说明

- 本次只处理 `TODO.md` 中第一个标题未以 `[DONE]` 标记的任务，完成后停止。
- `TODO.md` 是任务顺序、需求、依赖、验证与完成记录的唯一权威来源；`PLAN.md` 只在阶段级计划变化时更新。
- 不做开放式历史问题扫描；只处理会阻塞当前任务、使当前任务行为无效、或由当前任务引入/暴露且未排期的测试失败。
- 不记录不可审计的内部推理链；本文件记录可执行计划、关键判断、进度和变更原因。

## 初始步骤

1. 读取 `TODO.md`，定位第一个未 `[DONE]` 的任务，并完整阅读该任务的需求、依赖、验证要求和完成记录。
2. 检查最近提交信息是否明确提到与该任务直接相关的未完成问题；若相关，将其纳入当前任务或作为前置任务写入 `TODO.md`。
3. 根据任务范围读取必要源码、测试和文档；优先使用 `rg`/`rg --files` 定位相关文件。
4. 若任务可直接完成，按现有代码风格实现；若发现必须先补的具体前置问题，只在 `TODO.md` 插入最小前置任务并停止。
5. 修改前在本文件记录即将编辑的范围；实施时使用小而集中的补丁。
6. 运行验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`（完整测试超时不超过 30 分钟）。
7. 若验证发现未排期失败，修复或在 `TODO.md` 中排入必要前置/后续任务；不能在未处理失败时把当前任务标记为完成。
8. 完成后在 `TODO.md` 的任务标题前加 `[DONE]`，更新完成记录；仅当阶段计划变化时更新 `PLAN.md`。
9. 提交本次所有相关改动，提交信息包含任务编号与简明说明。
10. 停止，不继续下一个任务。

## 当前状态

- 已读取 `TODO.md` 并按标题 `[DONE]` 状态定位首个未完成任务：
  `M6-3 [TODO] Conversation 示例、README 与 crate 文档`。
- 已检查最近提交：`4b3f97c [M6-2] Add cross-adapter conversation request acceptance`，未见直接
  指向 `M6-3` 的未完成阻塞项。
- 当前工作树只有本计划文件变更。

## M6-3 执行计划

1. 梳理现有 `README.md`、`src/lib.rs` crate docs、`src/conversation/**` rustdoc 和
   `examples/`，找出与 M6-3 要求直接相关的陈旧描述或缺口。
2. 新增离线 `examples/conversation_core.rs`：使用 deterministic ids 和 normalized
   `Response`/`ToolResponse`，演示 user→assistant tool use→tool result→final assistant→commit、
   cancel 后继续 feed、valid Boundary/fork、compaction 后 `effective_view`、snapshot→restore
   一致性；不得访问真实 endpoint，也不得模拟 Agent loop/tool registry。
3. 更新根 `README.md` 与 `src/lib.rs`：清楚展示 Conversation Core 架构、identity 注入、
   pending/commit/cancel、Boundary/fork、projection/effective view、persistence；保留 Client
   endpoint 文档并继续指向根 `PLAN.md`/`TODO.md`。
4. 为缺少说明的公共 Conversation 类型、错误或方法补齐 rustdoc 和最小示例，重点说明：
   pending 不能 snapshot、Boundary 会 stale、id/time 外部注入、fork 共享 immutable prefix、
   runtime Agent/registry/summarizer/DB driver 不在本层。
5. 使用 `rg` 审查陈旧的“Client 层当前计划/任务”引用，只修正与当前文档语义冲突的引用。
6. 验证顺序：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
   `cargo run --example conversation_core`；`cargo test --doc`；
   `cargo test --all --all-targets`（1800 秒上限）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
   `git diff --check`。
7. 若验证发现未排期失败，先修复或把最小必要任务写入 `TODO.md`；成功后将 `M6-3` 标为
   `[DONE]` 并补充完成记录。
8. 提交本轮全部相关改动，提交信息使用 `[M6-3] ...`，然后停止。

## 进度更新

- 已确认 `M6-3` 为首个未完成任务。
- 已读取 README、crate docs、Conversation 顶层 API、Boundary/fork/head、Projection/Compaction、
  Snapshot/Persistence 与现有 public-API 集成测试 helper。
- 发现 README 中仍有一处“Conversation Core 正按任务顺序继续实现持久化端到端一致性验收”的
  陈旧状态描述；本次会修正为当前实现状态，同时保留根 `PLAN.md`/`TODO.md` 作为当前阶段入口。
- 即将编辑：
  - 新增 `examples/conversation_core.rs` 离线示例。
  - 更新 `README.md` 的状态、Conversation Core 用法和可运行示例列表。
  - 更新 `src/lib.rs` 与少量 Conversation rustdoc，补充最小可运行示例和边界说明。
- 已完成实现与文档更新：
  - 新增 `examples/conversation_core.rs`，覆盖 tool round-trip、cancel resume 后继续 feed、
    Boundary/fork、compaction effective view、snapshot→restore。
  - 更新 README 的当前能力状态、Conversation Core 离线用法和示例列表。
  - 更新 crate docs，并为 `Boundary`、`fork_at`、`CancelDisposition`、`snapshot` 补最小 rustdoc 示例。
- 已完成验证：
  - `cargo fmt --all`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo run --example conversation_core`
  - `cargo test --doc`
  - `perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  - `git diff --check`
- 已更新 `TODO.md`：`M6-3` 标题改为 `[DONE]` 并补充完成记录。
- `TODO.md` 更新发生在全量验证之后，属于文档记录变更；按任务规则无需重跑编译/全量测试。
- 最终 `git diff --check` 已再次通过。
- 下一步：查看工作树并提交本轮改动。
