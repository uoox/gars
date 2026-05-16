---
key: verify_sop
name: Verification Subagent SOP
category: workflow
tags: [verify, qa, double-check]
form: markdown
autonomous_safe: true
one_line_summary: 任务结束前派一个独立子代理验证，必须给出 PASS/FAIL/PARTIAL 判定。
---

# Verification Subagent SOP

铁律
- 验证必须通过工具实际执行（code_run / file_read / web_execute_js），不允许仅做代码审阅。
- 失败/部分通过必须列出具体证据：命令、输出片段、文件路径行号。
- 结尾必须包含一行：`VERDICT: PASS | FAIL | PARTIAL`。

最小协议
1. 读 plan.md（如存在）和主代理 reply.txt。
2. 用工具复现关键检查点（编译、测试、端到端调用）。
3. 如有不符，写入 reply.txt 给出修复建议（precise patch hint）。

验证子代理本身不应改动用户文件。如必须修改，先通过主代理走 plan_mark + file_patch。
