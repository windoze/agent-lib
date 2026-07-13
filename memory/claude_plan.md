# 执行计划 — M3-R Milestone 3 Review

## 选中的任务
`TODO.md` 第一个未完成任务 = **M3-R Milestone 3 Review**(M3-1..M3-3 全部 `[DONE]`)。
里程碑 3 阶段验收 Review。**review 任务不拆分**。

## 目标(TODO M3-R "做什么")
逐条核对并给出证据(有测试覆盖/无覆盖):
1. **pop 路由四条规则**有测试覆盖:
   - 本层兑现不冒泡(handler 在本层 perform,不 pop);
   - 本层无 handler → pop 给外层;
   - 顶层仍无 → 报 `UnhandledRequirement`;
   - 查找从发出者外层起(跳过自身,防即时环)。
2. **"运行模式 = scope 差异"**:同一 machine 挂/不挂 interaction handler 行为差异有测试。
3. 参考 driver 与 `DefaultAgentLoop` 在 text/tool/approval 的**等价性证据充分**。
4. `UnhandledRequirement` 是**分类错误**,不静默跳过或挂起。

## 方法
- 只读审阅,不改生产代码(除非发现阻塞当前任务的真实缺陷/失败测试)。
- 审阅文件:
  - `src/agent/drive.rs`(drain / pop 路由 / UnhandledRequirement / HandlerScope + 四 trait)
  - `src/agent/drive/reference.rs`(参考 driver 生产代码)
  - `src/agent/drive/reference/tests.rs`(6 个等价性测试)
  - drive.rs 的 pop 路由测试(本层不冒泡 / pop / 顶层报错 / 防即时环)
  - `src/agent/loop_driver/default/tests.rs`(legacy 基线,对照等价性)
- 对四条 Review 检查逐一定位测试,记录测试名与断言点;若缺口 → 按 Test Failure / No-Workaround
  策略处理(补测试或加前置任务)。

## 验证命令(顺序)
1. `cargo fmt --all`(纯 review 预期 clean;若无代码改动则确保 clean)
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --lib agent::drive`(聚焦 M3)
4. `cargo test --all --all-targets`(≤30min)—— 若本任务仅改 md 且上次全绿则可复用绿结果
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
6. `git diff --check`

## 进度
- [ ] 审阅 drive.rs pop 路由 + UnhandledRequirement,定位四规则测试
- [ ] 审阅 scope 差异(with/without interaction)测试
- [ ] 审阅 reference 等价性 6 测试 vs legacy
- [ ] 运行验证命令
- [ ] TODO.md 标 [DONE] + Review 完成记录,提交

## 结论
(待填)

## 进度(更新)
- [x] 审阅 drive.rs pop 路由 + UnhandledRequirement:四规则测试齐全
      (drain_fulfills_locally_without_popping / drain_pops_to_parent_when_local_scope_lacks_handler /
       drain_top_scope_without_handler_is_unhandled_requirement / pop_starts_from_outer_scope_skipping_the_emitter)
- [x] 审阅 scope 差异:attended = reference_approval_approve;补齐 headless 同机对照
      reference_headless_scope_surfaces_unhandled_approval(新增 1 测试)
- [x] 审阅 reference 等价性 6 测试 vs legacy:text/single/parallel/failure/approve/deny,committed + 通知序列一致
- [x] 运行验证:fmt clean / clippy clean / lib agent::drive 17 passed / all-targets 434 passed 0 failed /
      doc clean / diff --check clean
- [x] TODO.md 标 [DONE] + Review 结论完成记录

## 结论
M3-R 通过。四条 pop 规则、运行模式=scope 差异(新增 headless 同机对照)、参考 driver 6 类等价性、
UnhandledRequirement 分类错误——全部有测试覆盖且全绿。评审范围内补 1 测试(非 workaround)。
下一未完成任务 = M4-1(本次不启动)。
