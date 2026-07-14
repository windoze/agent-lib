# 执行计划 — M2-1 实现 script model、strict 模式与 call log

## 选中的任务
`TODO.md` 第一个未完成任务 = **M2-1**(M1-1/M1-2/M1-3/M1-R 均 `[DONE]`)。
HEAD=c403017,工作树 clean。非 Review 任务,**不拆分**(单一可验证单元:script model 层)。

## 任务要求(TODO.md M2-1)
- `script.rs` 定义 `StrictMode`:脚本耗尽 panic 或返回分类错误;默认返回分类错误,panic opt-in。
- 定义 `LlmStep`、`ToolStep`、`InteractionStep`、`ReconfigStep`。
- 定义 `CallLog<TRequest, TResultSummary>`:调用序号、请求摘要、结果摘要、完成顺序。
- 脚本按顺序匹配;tool/interaction 预留 key 匹配接口,首版仅顺序。
- 错误信息含 family、调用序号、脚本长度、可选 cassette/scenario label。
- 验证:顺序消费+call log;耗尽返回可断言错误;strict panic 仅 opt-in 时 panic;全套验证。

## 设计(对齐 docs/TESTABILITY.md §5.3 建议 API)
- `StrictMode { Error(default), Panic }`。
- `trait ScriptStep`: `const FAMILY: RequirementKindTag`;`into_result(self)->RequirementResult`;
  `match_key(&self)->Option<&str>`(预留,默认 None)。
- Step 类型(payload = 对应 family 的 RequirementResult 载荷):
  - `LlmStep`(Result<Response,ClientError>):`text/tool_use/response/error`。
  - `ToolStep`(Result<ToolResponse,ToolRuntimeError>):`ok/error/response/runtime_error`;key=provider call id。
  - `InteractionStep`(InteractionResponse):`answer/choice/approval/response`;预留 key。
  - `ReconfigStep`(Result<(),ToolRuntimeError>):`ok/error`。
- `Script<S: ScriptStep>`:内部 `Mutex<VecDeque<S>>`+dispatched 计数;`new/with_strict_mode/with_label`;
  `next_step(&self)->Result<S,ScriptError>`(顺序 pop,耗尽按 StrictMode)。
- `ScriptError::Exhausted{family,call_index,script_len,label}`,手写 Display(含全部字段+可选 label)。
- `CallLog<Req,Res>`:`Mutex<Vec<CallRecord>>`+completion 计数;`begin(req)->CallTicket`、
  `complete(ticket,res)`、`record(req,res)`、`len/is_empty/completed_len/with_records/records/requests`。
  `CallRecord{call_index,request,result:Option,completion_index:Option}`——分离 dispatch 与 completion 顺序,
  为 M5 并发乱序完成预留。
- prelude 追加 step/StrictMode/ScriptError/Script/CallLog 导出。

## 步骤
1. [ ] 实现 crates/agent-testkit/src/script.rs(含单测)。
2. [ ] 更新 prelude.rs 导出。
3. [ ] 验证:fmt --check → clippy -Dwarnings(两 crate)→ test -p agent-testkit → 全量 test --all --all-targets
       → doc -Dwarnings → diff --check。(本任务改代码 → 需跑全量)
4. [ ] TODO.md 标 M2-1 [DONE] + 完成记录。
5. [ ] 提交,停止。

## 进度/发现
- [x] 实现 script.rs:StrictMode、ScriptStep trait、LlmStep/ToolStep/InteractionStep/ReconfigStep、
      Script<S>(顺序消费 + 预留 match_key)、ScriptError::Exhausted(手写 Display 含全字段+可选 label)、
      CallLog<Req,Res>(begin/complete 分离 dispatch 与 completion 序号)。11 个新单测全过。
- [x] prelude.rs 追加 script 层导出。
- [x] 验证全绿:fmt --check;clippy -Dwarnings(root + testkit);test -p agent-testkit(25 lib + 2 smoke);
      test --all --all-targets(agent-lib 434 + testkit 25+2,0 failed,7 network-gated ignored);
      doc -Dwarnings(修两处 redundant explicit link 后 root + testkit 干净);diff --check 干净。
- [x] TODO.md M2-1 标 [DONE] + 完成记录。
- [ ] 提交并停止。
