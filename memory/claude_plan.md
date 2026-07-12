# 当前执行计划

## 目标与约束

- 本次调用只完成 `TODO.md` 中按顺序出现的第一个、标题未带 `[DONE]` 的任务，然后停止。
- `TODO.md` 是任务顺序、需求、依赖、验证和完成记录的唯一事实来源；`PLAN.md` 仅在阶段级计划确实变化时更新。
- 不做开放式历史缺陷扫描。只检查最新提交是否明确提到与当前任务直接相关的未完成问题，以及当前任务实施或验证中暴露的阻塞问题。
- 若发现必须先解决的具体前置问题，则以最少的新任务更新 `TODO.md`、保持当前任务未完成、提交任务清单调整后停止；不采用规避方案。
- 若实现完成，则依次格式化、严格 lint、运行完整测试和文档构建，并将任务标题标为 `[DONE]`、填写完成记录、提交全部相关改动后停止。
- 不泄露私有的逐 token 推理；本文件记录可审查的决策依据、假设、执行步骤、证据和进度。

## 分步执行计划

1. 读取 `TODO.md`，从头定位第一个标题未带 `[DONE]` 的任务，完整提取其需求、依赖、验收标准和完成记录要求。
2. 检查工作树、当前分支和最新提交信息：
   - 保护用户已有改动，不覆盖或回退无关内容；
   - 判断是否为上次中断后遗留的同一任务；
   - 仅处理最新提交中明确指出且与当前任务直接相关的未完成问题。
3. 阅读当前任务直接涉及的设计文档和代码/测试，建立需求到实现与测试的对应关系；避免无关范围扩张。
4. 完整实现当前任务。采用小而聚焦的补丁，每次关键修改后复读受影响区域；如根因明显影响同类场景，则修复整个已识别的问题类别。
5. 添加或调整覆盖正常路径、边界条件、错误路径和序列化/流式状态（按任务实际内容选择）的测试。发现规范不匹配时先判断能否在当前任务内正确修复；若不能，按前置任务规则处理。
6. 按规定顺序验证：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all --all-targets`（最长 30 分钟，单测试不得超过 1 分钟）
   - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
   对任何未被后续明确任务覆盖的失败，立即修复或在 `TODO.md` 中加入最小前置任务；未处理前不标记完成。
7. 验证通过后，更新 `TODO.md`：给当前任务标题加 `[DONE]`，填写实际实现内容、测试命令和结果。只有阶段级依赖或完成标准改变时才更新 `PLAN.md`。
8. 检查最终 diff 和状态，确认仅包含当前任务以及恢复同一任务所需的全部未提交文件；如 `PROMPT.md` 有意外改动，也按用户要求纳入提交且不回退。
9. 使用包含任务编号的清晰提交信息提交改动。提交后确认工作树状态与提交内容，然后停止，不读取或实施下一个任务。
10. 若 `TODO.md` 已无未完成任务，则执行其规定的最终复核、提交必要调整并创建 `endtag`；这只在确认所有任务标题均为 `[DONE]` 后发生。

## 当前进度

- 已创建初始执行计划。
- 已完整读取 `TODO.md`，确定首个未完成任务为 `M5-2 OpenAI Response 流式(SSE) → StreamEvent`；后续的 `M5-R` 本次不实施。
- 当前任务的必需产出：
  - 解析 Responses API SSE，并把 output item added / delta / done / completed 映射为统一 `StreamEvent`；
  - 使用 `item_id` / `output_index` 建立稳定 `BlockId` 关联；
  - 文本、reasoning、function-call arguments 分别映射为 `Delta::Text`、`Delta::Reasoning`、`Delta::Json`；
  - tool arguments 只累积，在完整边界发布 `ToolInputAvailable`，不对 partial JSON 提前解析；
  - 用真实录制 SSE fixture 覆盖事件序列及经唯一 `Accumulator` 折叠的一致性；
  - 增加默认忽略的真实流式文本与 tool-call 集成测试。
- 下一步：检查工作树与最新提交是否存在同任务遗留或直接相关的明确未完成问题，然后阅读 M5-1、Anthropic 流式实现及相关测试，设计最小且完整的同构实现。

## 已确认的实现依据与设计

- 工作树初始仅有本计划文件改动；最新提交 `0666b7b` 完成 M5-1，未声明与 M5-2 直接相关的遗留阻塞。
- 已读取 OpenAI 官方 streaming guide、Responses streaming events reference 与 `/v1/responses` OpenAPI 资料。确认核心 wire 生命周期为：
  - `response.created` / `response.in_progress`；
  - `response.output_item.added`，message 内容另有 `response.content_part.added`；
  - `response.output_text.delta|done`、`response.refusal.delta|done`；
  - `response.reasoning_text.delta|done` 与 `response.reasoning_summary_text.delta|done`；
  - `response.function_call_arguments.delta|done`；
  - `response.content_part.done` / `response.output_item.done`；
  - `response.completed`，以及合法的 `response.incomplete`、`response.failed`、`error` 终态。
- 已对配置的 Foundry endpoint 做两次 55 秒内的最小真实流探测，认证值未输出或落盘：
  - 文本流实际包含空 reasoning item、message/content part、两个 output text delta，最终 `response.completed` 含 usage 与 Azure `content_filters`；
  - 工具流实际包含 function-call item、5 个 arguments delta、arguments done、item done 和 completed，完整参数为 `{"city":"Tokyo"}`。
- 模块方案：在 `adapter/openai_resp/stream/` 下按 decoder / wire / normalizer / tests 拆分；SSE framing 采用与 Anthropic 一致的 terminal-on-error 模式，但状态机独立校验 Responses 的 item/content 层级。
- 稳定 id 方案：item 级 reasoning/tool 使用 provider `item.id` 映射；message 的每个 content part 使用 `item_id + content_index` 映射，避免多 content part 被错误合并；同时校验 `output_index` 与 `item_id` 一致。
- tool 参数纪律：delta 仅追加原始字符串并立刻产出 `Delta::Json`；只在 `response.function_call_arguments.done` 完整边界比较完整字符串、解析 JSON并产出 `ToolInputAvailable`；随后 item done 才产出 `BlockStop`。
- terminal 方案：复用 M5-1 的完整响应转换逻辑解析 completed/incomplete 内嵌 response，以得到一致的 usage、stop reason 和顶层 extra；流状态机校验最终 normalized content 与已观察的 item/part 内容一致，随后发出 Usage、ResponseMetadata、MessageStop。
- 错误/扩展方案：provider failed/error 映射为分类化 `ClientError`；未知合法 output item/content/event 不伪装成已支持块，而是在终态完整响应的逃生舱中保留证据。协议错序、重复、id/index 不一致、done 与 delta 内容不一致、partial JSON 和 premature EOF 均显式报错。
- 下一步：先用小补丁抽取可复用的完整 response value 解析入口，再逐个新增 stream wire、normalizer、decoder、transport和 trait 实现；随后添加脱敏 fixture 与聚焦测试。

## 实施进度

- 已抽取 `parse_response_value`，让流式 terminal snapshot 与非流式响应共享完整 output/usage/stop reason/extra 转换逻辑。
- 已新增 `adapter/openai_resp/stream/`：
  - `wire.rs` 对核心 Responses 事件提供 typed view，同时保留原始 JSON；
  - `normalizer/` 校验零起点连续 sequence、response/item/content 生命周期及冗余的 item id/output index；
  - `decoder.rs` 支持任意 HTTP 字节分片、UTF-8/SSE framing 错误分类及 terminal-on-error；
  - transport 校验 stream 请求模式、HTTP 错误/Retry-After 与 `text/event-stream` content type。
- `OpenAiRespAdapter` 已实现 dyn-safe `LlmClient` 的完整态与流式两条路径。
- 已加入 2026-07-13 实际 Foundry 文本/tool SSE 脱敏 fixture。14 个聚焦测试全部通过，覆盖：
  - 文本、空 reasoning、tool arguments 五段增量、usage、Azure `content_filters`；
  - 与 terminal 完整响应的 normalized 折叠一致性；
  - 两个 tool item 交错、reasoning text 与 encrypted signature；
  - partial JSON 只在 done 失败、sequence/event/id-index 错配、premature EOF、非法 UTF-8；
  - provider error/failed/incomplete 分类、未知 future event 逃生舱；
  - 本地 HTTP、Retry-After、content type、截断 body 与 `Box<dyn LlmClient>`。
- 已在 `tests/integration_openai_resp.rs` 新增默认忽略的真实流式文本和强制 tool-call 测试；通过 `direnv exec . cargo test --test integration_openai_resp -- --ignored --nocapture` 验证 3/3 通过（含既有非流式测试，总耗时 3.56 秒）。
- 已将原先过长的状态机代码拆分为 response normalizer、terminal reconciliation、event field 校验、message part、reasoning accumulation 与 JSON accessor 等聚焦模块；重组后聚焦测试仍为 14/14 通过。
- 最终验证结果：
  - `cargo fmt --all`：通过；
  - `cargo clippy --all-targets -- -D warnings`：通过，无 warning；
  - `cargo test --all --all-targets`：129 passed，6 ignored；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：通过；
  - 重组后再次运行 `direnv exec . cargo test --test integration_openai_resp -- --ignored --nocapture`：3 passed，3.80 秒；
  - `git diff --check`：通过（标记完成前结果，最终提交前会再次检查）。
- 已将 `TODO.md` 的任务标题更新为 `M5-2 [DONE]` 并填写实现与验证完成记录；阶段顺序和依赖未变化，因此未修改 `PLAN.md`。
- 下一步：复核最终 diff、任务边界和工作树，确认没有 secrets/无关改动并执行最终 diff check；随后以 `[M5-2]` 提交全部本任务改动并停止，不进入 `M5-R`。
