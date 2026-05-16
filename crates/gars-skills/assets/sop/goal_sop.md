---
key: goal_sop
name: Goal Mode SOP
category: workflow
tags: [goal, autonomous, budget]
form: markdown
autonomous_safe: true
one_line_summary: 目标模式：在预算时间内自驱推进开放目标，外层 driver 会在你提前停止时把你 nudge 回来继续。
---

# Goal Mode SOP

进入条件
- 用户给的目标偏开放（"调研一下 X"、"持续改进项目 Y"），同时给了时间预算或允许长时间运行。

工作流
1. 第一轮：读 ~/.gars/memory/* 和当前 cwd 的 README、TODO，把目标拆成可推进的几条线。
2. 每一轮：挑一条最值得推进的线，调用必要工具，把结果写回 ~/.gars/runs/<run_id>/reply.txt 末尾追加。
3. 不要追求一次"完成"。预算用完前都还在工作。如果你输出"任务完成"，外层 driver 会读取这条 SOP，再给你一个 nudge prompt，告诉你还剩多少秒、让你深化或验证之前的工作。
4. 你可以调用 plan_create 切换到规划模式去推进某条线；也可以调用 subagent_dispatch 派一个验证子代理。

护栏
- 每轮结束写一行进度到 reply.txt（即使被 nudge 也要保留历史）。
- 拒绝调用会"清空环境"、"重置数据"之类破坏性操作的工具，除非用户在 objective 里明确授权。
- 接近预算耗尽（剩余 < 10%）时，最后一轮做一次"收尾总结"而不是开新坑。

退出条件
- 预算用完（driver 强制停止），或
- 用户在外部写了 ~/.gars/runs/<run_id>/_stop 文件。
