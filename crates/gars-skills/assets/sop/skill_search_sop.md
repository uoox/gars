---
key: skill_search_sop
name: Skill Search SOP
category: meta
tags: [skill, search, reuse]
form: markdown
autonomous_safe: true
one_line_summary: 任务开始前先搜本地 Skill / SOP，命中即复用。
---

# Skill Search SOP

何时使用
- 接到任何稍复杂的请求（≥2 步、外部接口、生僻领域）先 search。
- 用户主动说"找个做 XXX 的 skill"。

工具
- skill_search(query, top_k=5, category=None)：本地 BM25 检索 ~/.gars/skills/**/*.md。
- skill_show(key)：读 frontmatter + 全文。
- skill_import(url|path)：把外部 skill 下载到 ~/.gars/skills/imported/。

匹配优先级
1. 完全匹配 key
2. frontmatter tags / category hit
3. 标题/正文 BM25

命中后必须读全文再决策，不能凭一行 summary 就调度。
