# 实施计划:LLM API Client 层

> 本计划落地 `docs/client-layer-references.md` 的结论,自研 Client 层(方案 C)。
> 上游设计依据:`DESIGN.md`(总体架构)、`docs/conversation-core.md`(Conversation 依赖的类型)、
> `docs/client-layer-references.md`(参考分工与类型草案)、`docs/genai-probe-findings.md`(为何自研)。
> 任务清单见 `TODO.md`。

## 范围与非目标

**范围**:LLM API Client 层——统一 message / content / streaming / usage / capability,
提供 Anthropic 与 OpenAI Response 两个 wire protocol 适配器,对上层暴露归一化模型。

**非目标(本计划不含)**:Conversation 层(committed log/pending/projection)、Agent 层、
Tool registry、多 agent 编排。这些依赖本层类型,后续单独立计划。

## 已定关键决策(实现时遵守,勿重新发明)

1. **运行时**:tokio;async trait 一律 `#[async_trait]`(dyn-safe 优先)。
2. **序列化**:数据类型(message/content/tool/response/usage/config)全部 `serde`;运行时资源不 serde。
3. **逃生舱三分**:(A) `ProviderExtras` 请求侧、绑 ProviderId,按需;(B) `#[serde(flatten)] extra` 响应侧自动兜底,先做;(C) `Normalized<T>` 枚举归一化 + 保留 raw,先做。
4. **块用稳定 `id` 关联,不用位置 `index`**。Anthropic 适配器负责把它的 `index` 映射成稳定 id。
5. **块三段式同构**:text/reasoning/tool_input 共用 `BlockStart{id,kind}` / `BlockDelta{id,delta}` / `BlockStop{id}`,`delta` 用枚举区分;Accumulator 只写一份。(替代方案:各自独立事件变体 —— 已否决,理由:Rust 枚举更适配、逻辑统一。)
6. **tool 参数流式**:delta 阶段只累积原始 JSON 文本,`ToolInputAvailable`(或 BlockStop)时才 parse。**绝不边流边 parse**。
7. **StreamEvent 只归一化 "LLM wire 上真实发生的事件"**;approval/abort/pivot 属 Agent 层 `AgentEvent`,不下沉到 client 层。
8. **Capability 结构化**,非布尔标志。
9. **Usage 是 Response 一等公民**,cache_read/cache_write/reasoning 单列字段。
10. **参考分工**:StreamEvent→Vercel AI SDK v5 taxonomy;ContentBlock→Anthropic 块分类;OpenAI Response 适配→async-openai;Message/Tool 组织→genai。抄分类学不抄传输层。

## 里程碑总览

| 里程碑 | 目标 | 产出 |
|---|---|---|
| **M1 基础数据模型** | 完整态类型 + 逃生舱 + serde | `Message`/`ContentBlock`/`Role`/`Usage`/`Normalized<T>`/`ProviderExtras` |
| **M2 流式事件与聚合** | 增量态 + 归一化事件 + 折叠 | `StreamEvent`/`Delta`/`BlockKind`/`Accumulator` |
| **M3 Client 抽象** | trait + capability + error + config | `LlmClient` trait、`Capability`、`ClientError`、`EndpointConfig` |
| **M4 Anthropic 适配器** | 打通真实 Anthropic wire | `AnthropicAdapter`(非流式+流式+tool),真实 endpoint 测试 |
| **M5 OpenAI Response 适配器** | 打通真实 Response wire | `OpenAiRespAdapter`,真实 endpoint 测试 |
| **M6 跨 provider 验收** | 归一化一致性 + 逃生舱实证 | 统一集成测试、能力矩阵、示例 |

依赖:M1 → M2 → M3 → (M4, M5 可并行) → M6。

## 完成状态

- 2026-07-13:Client 层 M1--M6 及各里程碑独立 Review 已全部完成。Anthropic
  Messages 与 OpenAI Responses 的完整态、流式、tool 往返和跨 provider 归一化均已通过
  真实 endpoint 验收;三类逃生舱、能力矩阵、可运行示例及 Conversation 层所需公共类型
  已齐备。后续工作由 `TODO.md` 的交接任务另行归档并建立 Conversation Core 计划。

## 测试与真实环境

- 单元测试:类型 serde round-trip、Normalized 映射、Accumulator 折叠、边界情况。
- **真实集成测试**:`.envrc` 提供两个 Foundry 代理 endpoint。
  - Anthropic wire:base `ANTHROPIC_BASE_URL`,认证 `Authorization: Bearer $ANTHROPIC_AUTH_TOKEN`,model `databricks-claude-haiku-4-5`,路径 `/v1/messages`。
  - OpenAI Response wire:base `OPENAI_BASE_URL`,认证 `api-key: $OPENAI_API_KEY`,query `?api-version=2025-04-01-preview`,model `gpt-5.5`,路径 `/responses`。
  - 集成测试用 `#[ignore]` 标记 + 读环境变量,缺变量时跳过,不阻塞 CI。
- 探测代码在 `probes/genai-probe/`,可作为真实调用的写法参考(resolver/auth 处理)。

## 目录结构(建议)

```
src/
  lib.rs
  client/
    mod.rs            # LlmClient trait, Capability, EndpointConfig
    error.rs          # ClientError 分类
  model/
    mod.rs
    message.rs        # Message, Role
    content.rs        # ContentBlock, 多模态承载
    tool.rs           # ToolCall, ToolResponse, Tool(schema)
    usage.rs          # Usage
    normalized.rs     # Normalized<T>, StopReason
    extras.rs         # ProviderExtras, flatten extra 约定
  stream/
    mod.rs            # StreamEvent, Delta, BlockKind, BlockId
    accumulator.rs    # Accumulator: StreamEvent -> Response
  adapter/
    mod.rs            # Adapter trait / dispatch
    anthropic/        # Anthropic wire
    openai_resp/      # OpenAI Response wire
tests/
  integration_*.rs
```

## 每阶段结束的 Review
每个里程碑末尾有独立 review 任务:核对本阶段产出与 DESIGN.md/conversation-core.md 约束一致、
测试覆盖完整、无遗留 TODO、公共 API 文档注释齐全,并显式确认下一阶段的依赖已满足。
