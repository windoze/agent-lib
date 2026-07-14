# 执行计划 — M2-3 实现 `TestScope` builder

## 选中的任务
`TODO.md` 第一个未完成任务 = **M2-3 实现 `TestScope` builder**(M1-* 与 M2-1/M2-2 均 `[DONE]`,
且 M2-2 已提交 `1edac17`)。HEAD=`1edac17`,工作树 clean。非 Review 任务,单一可验证单元
(scope 层),**不拆分**。

## 任务要求(TODO.md M2-3)
- `scope.rs` 实现 `TestScope` + `TestScopeBuilder`。
- builder 支持 `.llm(..)`、`.tool(..)`、`.interaction(..)`、`.subagent(..)`、`.reconfig(..)`。
- headless:未挂 interaction 时 `interaction()` 返回 `None`(默认即是)。
- attended helper 必须显式调用(提供 `.attended(..)` 别名,不默认兜底)。
- 支持 wrapping `ReferenceScope`(或任意 `HandlerScope`)/把已有 handler trait object 放入 scope。
- handler 用 `Arc` 存储,测试结束后可读 call log。

## 设计
- `TestScope` 存 `Option<Arc<dyn LlmHandler>>` 等五个 family 槽 + `inner: Option<Arc<dyn HandlerScope>>`。
- `HandlerScope` 五个 accessor:先看本层 override(`Some(arc.as_ref())`),否则委派 `inner`,否则 `None`。
  →「默认不 total」:未显式挂且无 inner 时返回 `None`,顶层缺 handler 仍暴露 `UnhandledRequirement`。
- `TestScopeBuilder`(`#[derive(Default)]`)每个 setter 取 `Arc<dyn XHandler>`(调用点自动 unsize 强转,
  调用方可持另一个具体 `Arc` clone 读 log)。`.attended(h)` = `.interaction(h)` 语义别名。
  `.wrapping(Arc<dyn HandlerScope>)` 设 inner。`.build()` 出 `TestScope`。
- `TestScope::builder()`、`TestScope::empty()` 便捷入口。prelude 追加导出 `TestScope`/`TestScopeBuilder`。

## 测试(scope.rs 内)
1. 空 scope 五个 accessor 全 `None`(`empty()`)。
2. 只挂 tool 时仅 `tool()` 为 `Some`,其余 `None`。
3. headless 顶层 scope 遇 `NeedInteraction` 仍 `UnhandledRequirement`:
   `default_machine` + `with_approval_policy(RequireApprovalPolicy)`(inline 测试 helper)+
   `agent_spec_with_tools([weather_tool])`;ScriptedLlmHandler 出 `tool_use`;TestScope 挂 llm+tool 不挂
   interaction;`drain(.., None, ..)` → `AgentError::UnhandledRequirement { kind: Interaction }`。
4. wrapping:`.wrapping(ReferenceScope)` 时 llm/tool/reconfig 委派到 inner(可选补测)。

## 步骤
1. [x] 实现 crates/agent-testkit/src/scope.rs(结构 + builder + HandlerScope impl + 4 单测)。
2. [x] prelude.rs 追加 TestScope/TestScopeBuilder。
3. [x] 验证全绿:fmt --check;clippy -Dwarnings(root + testkit);test -p agent-testkit(42 lib + 2 smoke);
       test --all --all-targets(agent-lib 434 + testkit 42+2,0 failed,7 network-gated ignored);
       doc -Dwarnings(修一处 redundant explicit link 后干净);diff --check 干净。
4. [x] TODO.md 标 M2-3 [DONE] + 完成记录。
5. [ ] 提交并停止。

## 进度/发现
- setter 取 `Arc<dyn XHandler>`:调用点由 `Arc<H>` 自动 unsize 强转,免 `as` 标注。
- accessor `arc.as_ref()` 得 `&dyn XHandler`(生命周期系于 `&self`);inner 委派 `and_then(|s| s.llm())`。
- wrapping 测试用内层 `TestScope`(非 `ReferenceScope`):testkit 不 mock `LlmClient`,两者同走
  `Arc<dyn HandlerScope>` 委派路径。
- headless 负例复用 reference `reference_headless_scope_surfaces_unhandled_approval` 的思路,
  用 inline `RequireApprovalPolicy` + scripted tool_use 驱动 `NeedInteraction`。
