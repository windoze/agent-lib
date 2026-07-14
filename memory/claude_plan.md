# 当前任务：M1-1 建立复杂测试支持模块与 MockPlanBlackboardStore

## 定位
- `TODO.md` 第一个未完成任务 = **M1-1**（首个 `[TODO]`）。前置依赖：无。
- 新一轮计划：复杂 Mock 测试与 Plan 依赖语义（PLAN.md / docs/complex-tests.md / docs/agent-layer.md §6.2）。
- HEAD=a1a94ee，工作树干净。上一轮 M1..M7 已归档。

## 目标（TODO.md M1-1）
新建：
- `tests/complex_support/mod.rs`（声明 `pub mod plan_blackboard;`）
- `tests/complex_support/plan_blackboard.rs`（内存 store + 类型 + 错误）
- `tests/agent_complex_support.rs`（`#[path="complex_support/mod.rs"] mod complex_support;` + 4 个测试）

数据模型：
- `MockPlanBlackboardStore { plan: Mutex<PlanState>, board: Mutex<Vec<BoardMessage>>, ops: Mutex<Vec<StoreOp>> }`
- `PlanState { id: PlanId, version: u64, task_order: Vec<String>, tasks: BTreeMap<String, TaskState> }`
- `TaskState { status: TaskStatus, owner: Option<String>, depends_on: Vec<String> }`
- `TaskStatus`: Todo/InProgress/Completed/Blocked/Cancelled
- `BoardMessage { offset: u64, sender: String, text: String }`
- `StoreOp { kind, outcome: Result<String,String> }`，成功/失败都记录

plan 操作：
- `create_plan` → 初始化 version=0 空 plan，记录 op
- `add_task(id, depends_on)` → 校验依赖已知/非自依赖/无环；成功追加 task_order，version+1
- `claim(task, owner, expected_version)` → CAS 版本 + owner + status + 依赖已完成；依赖未完成返回 DependencyBlocked 且不改状态（原子）
- `claim_first_available(owner, expected_version)` → 按 task_order 跳过 completed/已认领/依赖未完成，认领首个可用；无则 NoAvailableItem
- `update_status(task, owner, status, expected_version)` → owner/version/合法转换校验，成功 version+1

blackboard 操作：
- `post(sender, text) -> offset`（append-only，offset 从 0 单调递增）
- `read_from(offset) -> Vec<BoardMessage>`（offset 及之后）

错误：`StoreError` enum（UnknownTask/SelfDependency/DependencyCycle/DuplicateTask/VersionConflict/NotOwner/DependencyBlocked/AlreadyClaimed/NoAvailableItem/InvalidTransition），`Display` 产出 model-visible 文本，供 M1-2 tool adapter 使用。
`ops_summary()` 提供日志摘要，便于失败定位（M1-3 断言复用）。

环检测：deps 必须引用已存在 task（DAG by construction），add_task 内部仍跑 `detect_cycle`（防御 + 共享）。`detect_cycle` 设为 pub，测试用手工构造的 A→B→A 图断言其能识别环。

## 4 个必需测试（tests/agent_complex_support.rs）
1. `plan_dependencies_reject_unknown_self_and_cycles`
2. `claim_rejects_unfinished_dependencies_atomically`
3. `claim_first_available_skips_blocked_and_claimed_items`
4. `blackboard_is_append_only_and_offsets_are_monotonic`

## 验证顺序
- `cargo fmt --all -- --check`
- `cargo test --test agent_complex_support <各测试名>`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all --all-targets`（<=30min）
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- `git diff --check`

## 完成后
- TODO.md M1-1 标题 `[TODO]`→`[DONE]`，补完成记录。
- 提交：`[M1-1] ...`。停止。

## 进度
- [完成] store + 4 测试 + 完成记录, 全部验证通过, 待提交
