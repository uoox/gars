---
key: input_sop
name: Physical Input SOP
category: desktop
tags: [keyboard, mouse, automation]
form: markdown
autonomous_safe: false
one_line_summary: 跨平台物理键鼠操作，操作前先 dry_run，不要在用户活动时段乱点。
---

# Physical Input SOP

铁律
- 凡是 input_act 都默认有 dry_run；除非用户明确要求执行，先用 dry_run 验证目标。
- 不要在前台窗口标题不可预期时执行 click/type。

聚合工具
- input_act(action="click|move|type|key|screenshot", ...)：单一入口，避免工具炸开。
- 支持参数：x, y, button(left|right|middle), text, key(seq 如 "ctrl+s"), bbox=[x,y,w,h], dry_run=true

兼容性
- Linux 需要 X11 或 Wayland 支持；如失败请走 ADB（手机/平板）或浏览器 CDP 替代。
