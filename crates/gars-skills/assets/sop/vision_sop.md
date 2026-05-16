---
key: vision_sop
name: Vision API SOP
category: perception
tags: [vision, image, llm]
form: markdown
autonomous_safe: true
one_line_summary: 调用视觉大模型描述图片；必须先 crop，禁止整屏盲传。
---

# Vision SOP

规则
- 优先用本地 ocr_image / ocr_screen（更快、隐私）；仅当 OCR 信息不足时升级到 image_describe。
- 不允许整屏抓后送 vision API；必须传 bbox（窗口或局部区域）。
- 描述请求要带具体问题（“按钮文字”、“表格首列”），不要泛问“描述这张图”。
- 高分辨率截图先 resize 到长边 1280 内再送。

工具
- image_describe(path|bytes, prompt, backend?)
- ocr_image(path|bytes)
- ocr_screen(bbox)
