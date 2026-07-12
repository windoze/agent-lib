# 当前执行计划

## 目标与边界

- 以 `TODO.md` 为唯一的任务顺序与验收依据。
- 本次只完成第一个标题未带 `[DONE]` 的任务；完成并提交后立即停止，不进入下一项。
- `PLAN.md` 仅在阶段级计划、依赖或完成标准确实变化时修改。
- 不做开放式历史问题排查；只处理当前任务、其直接阻塞项、当前任务引入的回归，以及测试策略要求处理的失败。
- 本文件记录可审查的计划、依据、关键决策和进度；不记录模型私有的逐字思维链。

## 分步执行计划

1. 读取 `TODO.md`，严格按标题上的 `[DONE]` 状态识别第一个未完成任务，并完整提取其依赖、实现范围、测试与完成记录要求。
2. 检查仓库工作区状态与最新提交，只判断是否存在未提交的续作，以及最新提交是否明确提到与当前任务直接相关的未完成问题。
3. 阅读当前任务直接涉及的设计、代码与测试；先建立基线，不进行无关的历史缺陷扫描。
4. 若发现使当前任务无法按规格实现的具体前置缺陷：精确界定问题，以最少数量在 `TODO.md` 中加入前置任务、补充依赖；必要时才改 `PLAN.md`；提交该记录后停止。
5. 若无阻塞，按任务原定执行单元完整实现。采用小而聚焦的补丁，及时回读受影响代码；对同一根因明确影响的一类情况作类级修复，不引入临时绕过。
6. 添加或更新覆盖任务验收条件、边界行为和回归风险的测试及文档。
7. 按要求先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，然后运行任务指定测试及不超过 30 分钟的 `cargo test --all --all-targets`；如任务要求，再运行 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。任何未被明确排期的失败都必须修复或转化为前置任务，不能忽略。
8. 验证实现与任务清单逐项一致；在 `TODO.md` 的任务标题前加 `[DONE]` 并填写真实完成记录。仅在阶段计划变化时更新 `PLAN.md`。
9. 再次检查差异和工作区，确认没有误改或遗漏；若这是异常中断后的续作，按要求将现有未提交文件全部纳入同一次任务提交。
10. 使用清晰、包含任务编号的提交信息提交全部本次变更。确认提交成功后停止，不读取或执行下一任务。

## 当前进度

- 已在任何仓库检查或命令执行前建立本计划文件。
- 已读取 `TODO.md`；按标题状态识别出的第一个未完成任务是 **M6-1 `[TODO]` 归一化一致性集成测试**。
- 本次任务要求：在 `tests/` 中以同一套参数化断言覆盖 Anthropic 与 OpenAI Responses 的纯文本、多轮、tool call 往返；必须通过 `Box<dyn LlmClient>` 调用；测试断言层不得泄漏 provider 特判；有相应环境变量时两家都应通过。

## M6-1 针对性实施方案

1. 检查 `git status` 与最新提交说明，判断是否是未提交续作，以及最新提交是否明确留下与 M6-1 直接相关的问题。
2. 阅读现有两家真实集成测试、adapter 构造 API、`LlmClient`/`ChatRequest`/`Response`/tool 类型；复用项目既有环境变量约定、超时与跳过策略。
3. 设计一个 provider-neutral 的测试场景驱动器：仅在 client 构造层选择 provider，后续纯文本、多轮、tool 往返均接受 `Box<dyn LlmClient>`，并调用相同的结构断言辅助函数。
4. 对 tool 往返保留第一次 assistant tool call，模拟执行工具并以统一 `ToolResult` 内容回灌，再发起第二次请求；断言 id 关联、assistant 文本/工具块、stop reason 与 usage 的合理性。
5. 确保断言表达跨 provider 共同契约，不比较 provider 原始 id、具体文本、精确 token 数或 provider 专属 extra；若现有公共模型不足以无绕过地表达往返，则按阻塞策略处理而不缩窄测试。
6. 先运行新增集成测试的编译与聚焦验证；真实 endpoint 测试默认 `#[ignore]`，在环境配置可用时运行两家同一套场景，并保证每个 case 有小于 1 分钟的超时。
7. 依次运行格式化、严格 clippy、完整测试与文档警告检查；处理所有失败。
8. 将 M6-1 标题改为 `[DONE]` 并填写实现和验证记录；审阅差异后提交包含任务号的单一提交，然后停止。

## 当前下一步

- 已检查工作区与最新提交：除本计划文件外起始状态干净；最新提交为已完成的 M5-R，没有明确遗留与 M6-1 直接相关的问题，因此不是异常中断后的续作，也没有需要插入的前置任务。
- 已核对两家 request mapper：统一 `Message`/`ContentBlock` 可原样承载多轮 assistant 消息、tool call id 和 `Role::Tool` 的 `ToolResult`；OpenAI replay 元数据也随 block extra 保留，当前任务不存在模型边界阻塞。
- 已新增模块化的跨 provider 测试矩阵：endpoint/provider 分支仅位于 client 构造层；纯文本、多轮、tool 往返场景和结构断言均共享，并通过 `Box<dyn LlmClient>` 调用。
- 新增测试默认忽略，整个矩阵由 55 秒外层 deadline 约束；缺少任一 provider 配置时只跳过对应 target，不读取或输出 secret。
- 已执行 `cargo fmt --all`；首次严格 Clippy 编译发现一处 `nonminimal_bool`，属于新增断言的等价表达式 lint，已按建议改为 `Option::is_none_or`，没有规格或设计变化。
- 重新格式化与严格 Clippy 已通过；新增测试目标默认模式通过（1 个 opt-in 测试按预期 ignored）。
- 使用 `direnv exec .` 运行真实矩阵时，Anthropic 的三类场景全部通过；OpenAI 纯文本通过，但多轮 follow-up 返回 HTTP 400：assistant 历史被 request mapper 错误编码为 `input_text`，服务端明确只接受 `output_text`/`refusal`。这是直接阻塞当前任务的既有协议缺陷，不能通过缩窄或改写测试绕过。
- 已按根因进行类级修复：OpenAI user 文本/图片继续使用 `input_text`/`input_image`；assistant 文本改用 `output_text`，并从已保留的 response extra 恢复 `refusal` 类型；Responses 不支持的 assistant image 与未知 replay 文本类型在发网前返回明确协议错误；tool result 多模态仍使用 input 内容词汇。
- 已更新完整请求期望，并新增 assistant output/refusal、modeled 字段优先、assistant image 与未知 replay 类型的回归覆盖。
- 修复后验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；OpenAI request mapper 聚焦测试 6 项；真实跨 provider 矩阵 1 项（Anthropic 与 OpenAI 均完整跑完纯文本、多轮、tool 往返，17.57 秒）。
- 完整质量门通过：`cargo test --all --all-targets` 为 130 passed、7 ignored；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 成功。
- 已将 `TODO.md` 的 M6-1 标题更新为 `[DONE]` 并写入实现、直接阻塞修复和真实验证记录；阶段级计划未变化，因此不修改 `PLAN.md`。
- 最终 `cargo fmt --all -- --check` 与 `git diff --check` 通过；工作区仅包含 M6-1 的测试矩阵、OpenAI assistant 历史修复及回归测试、`TODO.md` 和本进度文件，没有 `PLAN.md`/`PROMPT.md` 或无关改动。
- 已创建单一提交 `[M6-1] Add cross-provider normalization acceptance tests`；提交后的只读检查确认 `main` 仅领先 `origin/main` 1 个提交且工作区干净。本次 M6-1 已完成，立即停止，不开始 M6-2。
