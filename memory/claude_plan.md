# 执行计划 — M4-R：Milestone 4 Review

## 选中的任务
`TODO.md` 第一个未完成任务 = **M4-R**（M1..M4-3 全 `[DONE]`；M4-R/M5.. 仍 TODO）。
前置 M4-1..M4-3（含 M4-2a）已完成。

## 任务性质
Review 任务（不可跳过、不可拆分）。核对三点 + 跑全套验证 + 结论写入完成记录。

## 三个核对点
1. **cancel = never-resume 的"受控丢弃 + 闭合"语义**：
   - 被弃子树都触发了 `Conversation::cancel_pending`。
   - "cancel 后仍可 feed"有测试。
2. **pivot/approval/cancel 收编为统一表现**（requirement + handler + 多喂 input）：
   - 旧三套并列机制（pivot queue / approval responder / cancel token 主体）已删除或降级。
3. **无 multishot / continuation 复制被引入**；多路径仍指向 `fork_at`。

## 验证顺序
`cargo fmt --all --check` → `cargo clippy --all-targets -- -D warnings` →
`cargo test --all --all-targets`（<=30min）。纯 review 若无代码改动可复用上次绿测。

## 进度
- [x] 选中 M4-R，读 TODO M4-R 与 M4-1..M4-3
- [x] 核对点 1：cancel=never-resume + cancel_pending + cancel后可feed 测试（通过）
- [x] 核对点 2：pivot/approval/cancel 统一，旧机制已删（通过）
- [x] 核对点 3：无 multishot/continuation，多路径走 fork_at（通过）
- [x] 跑全套验证：fmt/clippy/test(423 lib)/doc 全绿
- [x] TODO.md 标 [DONE] + 完成记录（写 review 结论）+ 提交
