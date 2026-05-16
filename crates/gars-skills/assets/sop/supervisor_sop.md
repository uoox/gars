---
key: supervisor_sop
name: Supervisor Agent SOP
category: workflow
tags: [supervisor, monitoring]
form: markdown
autonomous_safe: true
one_line_summary: 长任务下用监督子代理观测 output.txt，违约时通过 _intervene 注入指令。
---

# Supervisor Agent SOP

职责
- 周期性读 output.txt，识别死循环、越界改动、危险命令。
- 监视约束清单（每 N 步必须 plan_mark / 必须先 file_read 再 file_patch / 不可写程序目录等）。
- 命中违约时不直接停子代理，先通过 _intervene 给出精确修正指令。

升级路径
- _intervene 后 2 步仍未纠正 → 写 _stop 终止
- 出现敏感词（删除 ~、丢库、强推 main）→ 直接 _stop 并通知主代理
