# 当前任务:M3-4 增加首个离线 recorded replay 测试

## 目标(来自 TODO.md M3-4)
- 新增 `crates/agent-testkit/tests/cassettes/agent_weather_tool_roundtrip.json`(testkit crate 下等价路径)。
- cassette 覆盖 user -> LLM tool_use -> tool result -> LLM final text。
- 新增 replay 测试:`DefaultAgentMachine` + `CassetteLlmHandler` + `CassetteToolHandler` 跑完整 turn。
- 断言 committed conversation、handler call log、final cursor。
- 无需网络、credentials、真实 tool backend。

## 关键设计
- 新集成测试文件 `crates/agent-testkit/tests/cassette_replay.rs`。
- fixture 生成不靠手写:用 M3-3 `CassetteRecorder::update(path)` 包 `ScriptedLlmHandler`/`ScriptedToolHandler`
  跑一遍真实 `DefaultAgentMachine`,record 到磁盘。录制的 LLM 请求指纹必然与 replay 时机器产出的请求一致。
  - 该 regenerate 测试受 `AGENT_TESTKIT_UPDATE_CASSETTES=1` 门禁,CI 默认 Skipped 不写盘。
- replay 测试运行时读取(std::fs::read_to_string,非 include_str!,避免 fixture 未生成时编译失败)→
  Cassette::from_json_str → CassettePlayer → llm_handler()/tool_handler()(持 Arc 以便读 call log)→
  TestScope(仅 llm+tool)→ drain 完整 turn。
- 场景:weather_tool;LLM step1 = tool_use get_weather(Shanghai) usage(5,2);tool = ok "Sunny...";
  LLM step2 = text 最终答复 usage(6,4)。无 approval policy → 无 interaction。
- 断言:cursor.kind()==Done;conversation pending None、1 turn、4 消息(User/Assistant/Tool/Assistant text);
  最终文本;llm.log().len()==2 & completed==2;tool.log().len()==1;budget used==17 tokens。

## 步骤
1. [x] 读 TODO/PLAN/源码,确认 recorder/replay/scope/fixtures API。
2. [x] 写 tests/cassette_replay.rs(regenerate[update-gated] + replay + 共享 scenario 构造)。
3. [x] 建 tests/cassettes/ 目录;用 update 门禁跑 regenerate 生成 JSON fixture。
4. [x] 人工检查 JSON 可读、无 auth/endpoint/raw body。
5. [x] 聚焦跑 replay 测试确认绿。
6. [x] fmt → clippy(-D warnings)→ 全量 test --all --all-targets → doc(-D warnings)→ git diff --check。
7. [x] TODO.md 标 M3-4 [DONE] + 完成记录。
8. [x] 提交并停止。

## 备注
- docs/external-agent.md 为无关的未跟踪文件,HEAD 已是干净 M3-3 提交,非失败恢复场景,故本任务不纳入提交。
