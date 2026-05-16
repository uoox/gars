---
key: subagent_sop
name: Subagent Dispatch SOP
category: workflow
tags: [subagent, parallel, file-protocol]
form: markdown
autonomous_safe: true
one_line_summary: 通过子代理派发可并行的子任务，按文件协议读写 input/output/reply。
---

# Subagent Dispatch SOP

何时使用
- 步骤之间无强依赖且可并行（每个步骤 ≥1 分钟）
- 需要由专长子代理处理（如 verify、explorer、reviewer）
- 主代理上下文已重时把细节工作下放

工具
- subagent_dispatch(name, input, parallel=false, verbose=false)
- subagent_status(run_id)
- subagent_intervene(run_id, message)

文件协议（在 ~/.gars/tasks/<run_id>/<agent>/ 下）
- input.txt：主代理写入子任务输入
- output.txt：子代理 append 工具结果/思考；每一轮结束追加一行 `[ROUND END]`
- reply.txt：子代理最终回复（写入即结束）
- context.json（多步任务必备）：主代理写入结构化上下文，比如:
  ```json
  {
    "task_root": "/abs/path/to/task",
    "plan_path": "~/.gars/plans/<plan_id>/plan.md",
    "key_info": "prior step results, file paths the subagent needs",
    "round": 0
  }
  ```
  子代理读了这个 JSON 才知道项目根、计划文件、上下文。
- _stop：存在则子代理立即退出
- _keyinfo：主代理写入上下文注入（轻量、文本）
- _intervene：主代理 append 中途指令

`[ROUND END]` 标记：每轮的 output.txt 最后追加 `[ROUND END]` 一行，方便多 round
驱动按 marker 切割。单 round 子代理也会写，无害。

子代理 10 分钟无 reply.txt 自动 stop。verbose=true 时 output.txt 含原始 tool result。
