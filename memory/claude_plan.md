# 当前执行计划

## 目标与边界

- 以 `TODO.md` 为唯一任务顺序与完成状态来源，只执行其中第一个标题未带 `[DONE]` 的任务。
- 在实现、验证、任务记录更新和 Git 提交完成后立即停止，不开始下一个任务。
- 若发现与当前任务直接相关且阻塞规范实现的问题，优先修复；若本次无法直接修复，则在 `TODO.md` 中插入最少的前置任务、保持当前任务未完成、提交记录后停止。
- 不开展与当前任务无关的历史问题扫描，不用缩小范围、特殊分支或替代表示规避规范问题。

## 分步执行计划

1. 读取 `TODO.md`，按标题顺序识别第一个未带 `[DONE]` 的任务，完整提取其需求、依赖、验收条件和完成记录格式。
2. 检查最新一次提交说明及当前 Git 工作区状态，只判断是否存在与该任务直接相关的未完成问题或上次中断遗留；保留用户已有改动，并在恢复任务时将全部未提交文件纳入最终提交。
3. 只读取当前任务所需的设计、源码和测试上下文；若任务明确引用 `PLAN.md`、`DESIGN.md` 或其他文件，则按需核对，不做开放式排查。
4. 根据任务要求实现完整功能。编辑采用小而集中的补丁，每个关键修改后复读相关代码，避免跨文件大补丁和任务私有 workaround。
5. 为正常路径、边界情况、错误路径和序列化/兼容性约束补充或调整测试；发现根因影响同类场景时覆盖整个问题类别。
6. 按规定顺序验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后在不超过 30 分钟的超时下执行 `cargo test --all --all-targets`；依据任务要求补充文档构建或定向验证。任何未被后续任务明确安排的失败都必须在本任务修复或转化为前置任务。
7. 验证通过后，在 `TODO.md` 的任务标题前加 `[DONE]`，填写准确的完成记录、实现摘要与验证结果。仅当阶段级计划确实变化时才更新 `PLAN.md`。
8. 复查 diff、工作区和任务边界，确认没有误改或遗漏；使用清晰、包含任务编号的提交消息提交所有本次应纳入的未提交文件。
9. 确认提交成功且当前任务已显式标记 `[DONE]`，记录最终状态并停止，不触碰下一任务。

## 当前进度

- 已完成：在执行任何仓库读取或命令前建立本计划文件；已读取 `TODO.md` 并锁定第一个未完成任务 `M2-3`。
- 当前任务：`M2-3 [TODO] Accumulator: StreamEvent → 完整 Response`。
- 本任务验收点：
  - 定义 provider-neutral `Response { message, usage, stop_reason, extra }`。
  - 在 `stream/accumulator.rs` 实现以 `HashMap<BlockId, PartialBlock>` 保存状态、以 `Vec<BlockId>` 保存开始顺序的单一折叠逻辑。
  - Text/Reasoning 累加同类字符串；ToolInput 只累加原始 JSON 字符串，在 `ToolInputAvailable`（优先）或 `BlockStop` / `finish` 时完整解析，失败返回明确错误且不 panic。
  - 实现 `push`、`finish` 及消费异步流的 `collect`。
  - 测试交错 block、并行/分片工具输入、三段 JSON、残缺 JSON，以及空流、仅 usage、错误事件等边界。
- 下一步：检查最新提交与工作区，随后读取 M2-3 直接相关的设计、模型、流事件和现有测试，确定错误类型、模块归属和公开 API 的最小一致方案。

## 已确定的实施方案

- 仓库基线：最新提交 `fc0f99c` 为已完成的 `[M2-2] Implement normalized stream events`，未声明与 M2-3 相关的遗留问题；本次开始时除本进度文件外无未提交修改。
- `Response`：放入独立的 `client/response.rs`，并从 `client::Response` 重导出。该类型派生 serde，`extra` 使用 flatten 响应逃生舱，便于后续 M3 trait 与 M4/M5 适配器共用。
- 聚合状态：记录 message role、按 id 索引的 partial block、BlockStart 顺序、累计 usage 与 stop reason。块内容在 `finish` 时按开始顺序转成 `ContentBlock`。
- 状态校验：明确拒绝重复 block id、未知 id 的 delta/stop、delta 类型与 block kind 不匹配、重复 stop/available、未闭合块、缺失 message start/stop 等协议错误。
- 工具 JSON：delta 仅做字符串拼接；`ToolInputAvailable` 提供的完整值优先；否则在 `BlockStop` 解析，若流在 stop 前结束则 `finish` 也尝试解析并返回具体 JSON 错误，绝不 panic。
- 异步收集：`collect` 接受带外层 `Result` 的通用 futures stream，以 `CollectError<E>` 区分上游流错误与 `AccumulatorError`，保持对未来 `ClientError` 的兼容。
- 测试结构：在 response 模块覆盖 serde/extra；在 accumulator 模块覆盖交错 text/reasoning/tool、三段 JSON、available 优先、缺尾、空流、仅 usage、错误事件、上游错误和 id/kind 协议校验。

## 当前进度（更新）

- 已完成：
  - 任务识别、最新提交核对、干净基线确认、相关设计与现有类型审阅、实现方案确定。
  - 新增 serde `client::Response` 与 flatten `extra` 逃生舱。
  - 实现 `Accumulator`、分类化 `AccumulatorError`、保留上游错误的 `CollectError<E>` 以及异步 `collect`。
  - 实现 text/reasoning/tool 三类块的统一 id 聚合、开始顺序恢复、usage 合并、stop reason/role 收集，以及工具 JSON 在 complete boundary 的延迟解析。
  - 将测试按 folding/errors/collect 拆分，避免生产文件与测试文件过长。
  - `cargo fmt --all` 与 `cargo clippy --all-targets -- -D warnings` 已通过；Accumulator focused tests 11/11、Response focused test 1/1 通过。
- 正在进行：执行完整测试与 rustdoc 验证；通过后更新 `TODO.md` 完成记录，再做最终 diff 审查与提交。

## 验证与收尾状态

- 完整验证已通过：`cargo test --all --all-targets` 共 49 个测试通过，0 失败；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 成功。
- `TODO.md` 已将 `M2-3` 标题改为 `[DONE]`，并记录 Response、Accumulator、延迟 JSON 解析、异步 collect、测试覆盖与验证命令。
- `PLAN.md` 未修改：本次实现遵循原有 M2 产出与 M2→M3 顺序，没有改变阶段依赖、假设或完成标准。
- 剩余步骤：复查所有 diff、运行最终 `git diff --check` 与状态检查，必要时仅修正文档/格式问题；随后提交全部本次未提交文件并确认提交状态，立即停止。

## 提交前确认

- 最终任务顺序复查通过：`M2-3` 已显式 `[DONE]`，下一个未完成标题为 `M2-R`，本次未开始该 review 任务。
- 最终工作区复查通过：改动仅涉及 M2-3 的 Response、Accumulator、测试、`TODO.md` 和本进度文件；`PLAN.md` 未变化。
- `git diff --check` 通过；实现文件 446 行，测试已拆为 58/175/147 行的 focused 文件及 40 行共享测试模块，没有继续保留 800+ 行的混合源文件。
- 下一步：暂存全部本次文件，检查 staged diff 与空白错误，创建 `[M2-3]` 描述性提交并确认工作区干净，然后停止。

## 决策依据摘要

- `TODO.md` 中 `M1`、`M2-1`、`M2-2` 均已显式 `[DONE]`，第一个未带 `[DONE]` 的标题是 `M2-3`，因此不能跳到 review 或 M3。
- `M2-3` 本身已是预定执行单元，当前没有证据表明需要拆分；默认完整实现并提交该任务。
- `M3-1` 才会引入正式 `ClientError`，所以本任务需要依据现有边界选择清晰但不过度预占 M3 设计的聚合错误表示；必须先检查现有 `StreamEvent::Error` 占位和公共模块结构。
- `PLAN.md` 把 Request/Response 作为 Client API 消费的数据，同时 M3 的 `LlmClient::chat` 直接返回 `Response`；将其公开为 `client::Response` 可避免把运行时折叠逻辑塞入完整态内容模块。
- 现有 `Usage::merge` 明确用于 stream aggregation，因此多个 `Usage` 事件按增量合并；Response 的 `extra` 在纯归一化事件流中为空，后续非流式适配器可直接填充 provider 未建模字段。
