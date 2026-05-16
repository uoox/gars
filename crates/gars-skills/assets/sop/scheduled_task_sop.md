---
key: scheduled_task_sop
name: Scheduled Task SOP
category: ops
tags: [schedule, cron, idle]
form: markdown
autonomous_safe: true
one_line_summary: 长时间没人发任务的时候，跑归档/反思/巡检；要可中断。
---

# Scheduled Task SOP

触发
- 服务空闲达到 idle_secs（默认 1800）
- 用户配置的 cron（在 schedules 表）

可做的事
- 归档 L4 未压缩会话
- 反思最近 N 个任务的失败模式，更新 L1/L2
- 巡检 ~/.gars 容量、临时文件清理

禁止
- 不可与活跃任务并发改写同一文件
- 不可发送外部消息（除非有 dry_run 关闭确认）
