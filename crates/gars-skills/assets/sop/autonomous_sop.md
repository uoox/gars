---
key: autonomous_sop
name: Autonomous Loop SOP
category: ops
tags: [autonomous, agent]
form: markdown
autonomous_safe: true
one_line_summary: 无人值守模式下，必须有明确退出条件和频率上限。
---

# Autonomous Loop SOP

铁律
- 每个 autonomous 循环必须声明 max_iters 和 max_wallclock_secs。
- 写入的所有变更要可回滚（git 或显式备份）。
- 每轮结束写 reply.txt 摘要，给 supervisor 检查。

频率
- 默认两轮之间 sleep ≥ 60s。
- 同一资源的写操作之间 sleep ≥ 5s。
