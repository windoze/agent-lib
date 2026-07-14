# 执行计划 — M2-4 实现 `ScriptMachine` machine double

## 选中的任务
`TODO.md` 第一个未完成任务 = **M2-4 实现 `ScriptMachine` machine double**
(M1-* 与 M2-1/2/3 均 `[DONE]`,HEAD=`ca39873` 已提交 M2-3,工作树 clean)。
非 Review 任务,单一可验证单元(machine 层),**不拆分**。

## 任务要求(TODO.md M2-4 + docs/TESTABILITY.md §5.7)
- `crates/agent-testkit/src/machine.rs` 实现 `ScriptMachine: AgentMachine`。
- external input 后吐固定 requirement batch,cursor 设为可被 `drain` 识别的非 terminal waiting state。
- 按 requirement id 记录 resume order、resume result tags、abandon count。
- 所有 outstanding resume 后进入 `LoopCursor::Done`(可配置 `done_after_all_resumed`)。
- abandon 后进入 `Idle`(`idle_on_abandon`)或 builder 指定的其它 cursor(`abandon_cursor`)。
- builder:requirements、done_after_all_resumed、idle_on_abandon、initial cursor(waiting cursor)、label。
- 支持嵌套测试的 child machine → 观察记录放共享 `Arc<ScriptMachineLog>`。

## 设计
- `ScriptMachineLog`(Arc 共享,interior mutability):`resume_order: Mutex<Vec<RequirementId>>`、
  `resume_tags: Mutex<Vec<RequirementKindTag>>`、`abandon_count: AtomicUsize`;
  accessor `resume_order()`/`resume_tags()`/`abandon_count()`/`resume_count()`。
- `ScriptMachine`:`cursor`、`waiting_cursor`(= builder 的 initial cursor,默认 streaming_step 固定 step id)、
  `batch: Vec<Requirement>`、`outstanding: BTreeSet<RequirementId>`、`done_after_all_resumed: bool`、
  `abandon_cursor: Option<LoopCursor>`、`label: String`、`log: Arc<ScriptMachineLog>`。
- `step`:
  - External → 重置 outstanding=batch ids,cursor=waiting_cursor,emit batch(quiescent true)。
  - Resume(known id) → log 记录 order/tag,移除 outstanding;空且 done_after_all_resumed → Done(Completed)。
  - Resume(unknown id) → cursor=Error(诊断信息含未知 id),不动 outstanding。
  - Abandon → abandon_count+1;若 abandon_cursor 有值则 cursor=该值。
- `ScriptMachineBuilder`(Default):`requirements(iter)`(extend)、`requirement(one)`、`done_after_all_resumed()`、
  `idle_on_abandon()`(= abandon_cursor(Idle))、`abandon_cursor(LoopCursor)`、`initial_cursor(LoopCursor)`、
  `label(into String)`、`build()`。`ScriptMachine::builder()` 入口。
- prelude 追加导出 `ScriptMachine`/`ScriptMachineBuilder`/`ScriptMachineLog`。

## 测试(machine.rs 内)
1. batch emit + out-of-order resume 完成:mixed NeedTool+NeedInteraction,乱序 resume,验 resume_order/tags、Done。
2. unknown resume id → Error cursor,outstanding 保留。
3. abandon 可配置:idle_on_abandon → Idle;未配置 → cursor 不变;abandon_cursor(Done) → Done。
4. drain + TestScope 本地 tool fulfillment smoke:ScriptMachine 出 NeedTool,TestScope 挂 ScriptedToolHandler,
   drain 跑到 Done,tool_log len 1,log resume 记录正确。

## 步骤
1. [x] 实现 machine.rs(ScriptMachineLog + ScriptMachine + builder + 5 单测)。
2. [x] prelude 追加导出。
3. [x] 验证全绿:fmt --check;clippy -Dwarnings(root + testkit --all-targets);test -p agent-testkit(47 lib + 2 smoke);
       test --all --all-targets(agent-lib 434 + testkit 47 + 2,0 failed,7 network-gated ignored);
       rustdoc -Dwarnings --workspace 干净;git diff --check 干净。
4. [x] TODO.md 标 M2-4 [DONE] + 完成记录。
5. [ ] 提交并停止。

## 进度/发现
- observability 用共享 `Arc<ScriptMachineLog>`(interior mutability)而非 machine 直存 Vec,才能覆盖
  「machine 被移入 driver / 作为 nested child」的读回场景(docs §5.7)。
- `done_after_all_resumed` 设为 opt-in:不调用则永不 terminal,供手动 step 驱动;drain 测试须显式调用。
- `abandon_cursor(LoopCursor)` 泛化 idle_on_abandon,一并覆盖 ParentBatchMachine 的 abandon→Done。
- `initial_cursor` = 非 terminal waiting cursor,默认 streaming_step(固定 step id 常量,免引 id source)。
- machine 不做 accepts_resolution 校验(drain 已在 fulfill 时用 RequirementKind::accepts 校验 family);
  故 stray/unknown resume id → Error cursor 是唯一诊断点。
