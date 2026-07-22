## Execution Plan — M2-R：M2 review（非流式响应侧正确性核对）

TODO.md 第一个未完成任务：**M2-R**（标题 `[TODO]`，review 任务，不新增功能）。
前置 M2-1（d01f398）+ M2-2（ea52ff6）均已 `[DONE]`。本任务独立核对 M2 正确性与完整性，
产出为 TODO.md M2-R 下方追加的 review 记录。

### 核对清单（TODO M2-R）

1. 设计文档 §4.3 逐条对照：`object` 校验、`choices[0]`、三种 content 落点、
   arguments 解析失败降级、`finish_reason` 全表、extra 兜底。
2. 确认 `Usage` 零改动（usage.rs 无 diff）且 cached/reasoning details 有测试钉住。
3. 确认 `src/adapter/common/` 与 `src/client/error.rs` 零改动（本里程碑只允许新增 openai_chat/ 内文件）。
4. fixtures 与 openai_resp 惯例一致（include_str! 加载、脱敏）。
5. 跑全量门禁命令，全部通过。

### 执行步骤

1. [x] 读 §4.3 + M2 实现（response.rs / convert.rs / tests/{mod,parsing,transport}.rs / 3 fixtures）。
2. [x] git diff M1-R(401bdd8)→HEAD 核对 usage.rs / common/ / error.rs 零改动。
3. [x] 对照 openai_resp response 测试惯例（目录结构 + include_str!）。
4. [x] fixtures 脱敏检查（grep 无真实 key/token/账号）。
5. [进行中] 全量门禁：fmt / clippy(默认) / clippy(external features) / test --all / doc。
6. [ ] TODO.md M2-R 追加 review 记录（逐条核对结论 + 门禁摘要 + 发现问题处置）。
7. [ ] M2-R 标 `[DONE]`。
8. [ ] git commit + stop。

### 核对结论（草案，待门禁验证后定稿）

1. **§4.3 逐条对照（已逐行核实）**：
   - `object == "chat.completion"` 校验（response.rs:74-91 四态 match）✓；
   - 取 `choices[0]`（read_choice:122-156，缺失/空/非 object/缺 message 均报错）✓；
   - content→Text（convert.rs:54-59）/ reasoning_content→Thinking{signature:None}（:44-52）/ tool_calls→ToolUse（:61-70）✓；
   - arguments 解析失败→input=null + extra[RESPONSE_EXTRA_KEY]["raw_arguments"]=原文（parse_arguments:175-192）✓；
   - finish_reason 全表 stop/length/tool_calls/content_filter/其它/缺失（normalize_finish_reason:81-90）✓；
   - 未建模字段进 extra（response.rs:105 extra=wire，choices 含 logprobs 保留）✓。
2. **Usage 零改动**：git diff 401bdd8→HEAD -- src/model/usage.rs 空 ✓；
   cached/reasoning details 由 text fixture（cache_read=4/reasoning=0）+ reasoning fixture（reasoning=35/cache_read=6）钉住 ✓。
3. **common/ 与 error.rs 零改动**：git diff 均空 ✓；M2 仅新增 openai_chat/ 内文件。
4. **fixtures 惯例一致**：目录结构 tests/{mod,parsing,transport}.rs + fixtures/*.json 与 openai_resp 同构；
   include_str! 加载（tests/mod.rs:42-48）✓；脱敏（grep 仅命中 `*_tokens` 字段名，无真实 key）✓。
5. **门禁**：待运行。

### 进度日志

- [x] 上下文读取 + §4.3 对照
- [x] 零改动 git diff 核对（usage.rs / common/ / error.rs 全空）
- [x] openai_resp 惯例对照 + fixtures 脱敏
- [x] 全量门禁（fmt 无 diff / 默认+external clippy exit 0 / test --all 全 0 failed / doc exit 0）
- [x] StopReason::unknown_value()==Other 核对（finish_reason 表逐行吻合 §4.3）
- [x] TODO review 记录 + [DONE]
- [ ] git commit + stop
