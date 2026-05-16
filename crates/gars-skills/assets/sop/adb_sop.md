---
key: adb_sop
name: ADB UI SOP
category: mobile
tags: [adb, android, automation]
form: markdown
autonomous_safe: false
one_line_summary: 通过 ADB 抓 UI 树、tap/swipe/text，关键点：先 dump 再决策，不盲点。
---

# ADB SOP

前置
- 本机已装 adb 并 PATH 可见。
- 设备已授权（`adb devices` 能看到）。

工具
- adb_ui(serial?, keyword?, clickable_only?)：解析 uiautomator dump，按 keyword 过滤。
- adb_tap(serial?, x, y)
- adb_swipe(serial?, x1, y1, x2, y2, ms=300)
- adb_text(serial?, text)

最佳实践
- 每次操作前 adb_ui 重新 dump，不要相信屏幕状态没变。
- 找元素优先用 keyword（按文字），其次 resource_id，最后坐标。
- 弹窗（FrameLayout 全屏 + 关闭 X）必须先关。
- 已知应用包名（部分）：com.sankuai.meituan.takeoutnew, com.taobao.taobao。
