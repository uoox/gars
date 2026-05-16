---
key: trigger_sop
name: Trigger Mode SOP
category: workflow
tags: [trigger, reflect, schedule]
form: markdown
autonomous_safe: true
one_line_summary: 触发任务：每次 tick 先判断 YES/NO 条件，命中再跑动作；不命中则什么都不做、等下一次 tick。
---

# Trigger Mode SOP

触发模式本质是一种高频 schedule —— 实现层面就是定时调用，只是 prompt 强制按"先判断、再动作"的二段式来写。

工作流（你会被多次调用，每次都是一次独立的 tick）
1. 用户传入的 prompt 形如：
   ```
   # 触发器
   先回答（仅输出 YES 或 NO）：{{condition}}

   如果你输出 YES，则执行：{{action}}
   ```
2. 第一步：用必要工具评估 condition，得到布尔判断。
3. 第二步：如果是 NO，**直接输出 `NO` 并结束这一轮**（不要解释、不要调用别的工具，留给下一次 tick）。
4. 第三步：如果是 YES，先在回复开头写一行 `YES`，然后照 action 描述完整地执行任务，正常调用工具。

护栏
- condition 不要重复"上次已经处理过"的事件。可以读 ~/.gars/runs/<run_id>/last_trigger.txt 记录上次命中的指纹，避免重复触发。
- 如果用户设置了"触发后停止"（ONCE 模式），命中并完成一次 action 后调用一次 `exit_loop` 类工具，外层 scheduler 会把这个任务标记为 done。
- 如果连续 N 次都命中（异常情况，比如条件没改），主动 warning 一次并暂停一段时间。

退出条件
- 用户写了 _stop 文件，或者
- ONCE 模式下命中一次后正常结束。
