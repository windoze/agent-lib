# 当前任务:M3-R Milestone 3 Review（cassette 录制/离线重放里程碑复盘）

## 目标（来自 TODO.md M3-R）
- 核对 cassette schema 是否 provider-neutral。
- 核对 redactor 默认策略。
- 核对 record/update 环境变量护栏。
- 核对 replay 测试在无 credentials 环境可跑。
- 更新 `docs/TESTABILITY.md` 中任何与实现不一致的 cassette 描述。
- 验证:全套验证命令通过;Review 结论写入完成记录。

## 复盘结论（已核对源码）
1. provider-neutral schema（通过）:Cassette{schema_version, metadata, entries, observations};
   entry 只记 ChatRequest/ToolCall/Interaction/ToolSetRef 与 Response/ToolResponse/
   InteractionResponse/reconfig ok/error——全是 effect 边界 provider-neutral 类型,无 header/auth/endpoint/raw body。
2. redactor 默认策略（通过,doc 措辞需修正）:DefaultRedactor 把 provider_extras.fields 与
   Response.extra 中非白名单字段的值换成 <redacted>,保留 key 形状,message 文本保留,白名单默认空。
   doc 原写"移除字段",与实现(保留 key、脱敏 value)不符 → 修。
3. env 护栏（通过）:RECORD_ENV_VAR=AGENT_TESTKIT_RECORD_CASSETTES、
   UPDATE_ENV_VAR=AGENT_TESTKIT_UPDATE_CASSETTES;写模式仅 override/env=="1" 才 enabled;
   Verify 永不写盘;finish() 对未启用写模式返回 Skipped 不写文件。
4. replay 无 credentials（通过）:replay handler 是终端 handler,无 delegate、无网络;集成测试
   cassette_replay.rs::offline_replay_runs_a_full_weather_turn 跑完整 turn 证明。

## doc 与实现不一致（需修 docs/TESTABILITY.md,全部 cassette 相关）
- Cassette::load(path) 不存在 → 实际 std::fs::read_to_string + Cassette::from_json_str(&json)。
- CassetteLlmHandler::replay(cassette) 等不存在 → 实际 CassettePlayer::new(cassette,label).llm_handler()/.tool_handler()/.interaction_handler()（或 CassetteXHandler::from_cassette）。
- §5.4 recorder 示例把 wrap_llm(..) 结果命名为 recorder(实为 RecordingLlmHandler)→ 拆开写。
- fingerprint 描述 "canonical JSON hash" → v1 实际用 volatile-id-normalized canonical JSON 串(后续可换 hash)。
- §5.4 录制内容 metadata 列 created_at、单独 "run inputs" section → 实现无;metadata 仅 test_name/可选 description/可选 crate_version;scenario label 归入 description。
- §5.4 匹配策略 "可选自定义 key 匹配" → 当前只做 fingerprint 匹配,注明为后续扩展。

## 步骤
1. [x] 读 TODO/PLAN/cassette 源码/TESTABILITY,定位不一致点。
2. [x] 编辑 docs/TESTABILITY.md §5.4 与 §6 的 cassette 描述,使与实现一致。
3. [x] cargo fmt --all --check(docs-only,预期 clean)。
4. [x] 聚焦跑 cassette 测试实证 review 结论:cargo test -p agent-testkit cassette +
       cargo test -p agent-testkit --test cassette_replay(<1min,验证 env 护栏 + 离线 replay)。
5. [x] 全套验证:自 M3-4(HEAD=1946a77)全绿后仅改文档(*.md),按"仅文档变更复用上次绿"规则跳过重跑,完成记录注明。
6. [x] TODO.md 标 M3-R [DONE] + 完成记录(含 Review 结论 + doc 修订清单)。
7. [x] 提交并停止。

## 备注
- docs/external-agent.md 为无关的未跟踪文件(HEAD 干净于 M3-4,非失败恢复场景),本任务不纳入提交。
- 不新增/拆分任务:未发现阻塞当前任务的 spec 偏差或未排期失败测试。
