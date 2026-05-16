---
key: plan_sop
name: Plan Mode SOP
category: workflow
tags: [plan, multi-step, checkpoint]
form: markdown
autonomous_safe: true
one_line_summary: 多步骤任务进入计划模式，按 8 态步骤标记 + 5 阶段流程推进。
---

# Plan Mode SOP

## 进入条件

任何满足下列任一项的任务都应进入 plan mode：
- ≥3 个有依赖的步骤
- 多文件修改
- 用户显式说"做计划/plan"
- 结果不可逆且需独立 verify

不要给 1-2 步的任务硬套 plan。

## 步骤标记集（8 态）

| 标记      | 状态     | 何时使用                                                                 |
|-----------|----------|--------------------------------------------------------------------------|
| `[ ]`     | Pending  | 还没开始的步骤（初始状态）                                                |
| `[✓]`     | Done     | 完成；`note: <一句话结果>` 必填                                          |
| `[D]`     | Delegate | 大段代码阅读 / 网页抓取 / ≥3 次重复 / 测试分析——派 subagent              |
| `[P]`     | Parallel | Map 模式并行执行的占位（多条数据各跑一遍）                                 |
| `[?]`     | Question | 条件分支——执行前要根据上一步结果决定走哪条路                              |
| `[FIX]`   | Fix      | verify 失败后插入的补救步骤                                              |
| `[SKIP]`  | Skip     | 因上游依赖失败而跳过                                                     |
| `[✗]`     | Failed   | 失败且不再重试                                                           |

## 五阶段流程

### 1. 探索态（Exploration）
派 explorer subagent 摸清环境。**只读、不改**。可调 `file_read` / `web_scan` / `skill_search`。

### 2. 规划态（Planning）
- 用 `plan_create(dir, steps[])` 在 `~/.gars/plans/<run_id>/plan.md` 写入步骤
- 每条 `- [ ] N. <step title>`
- 引用相关 SOP 时在步骤末尾写 `(see plan_sop.md / verify_sop.md)`
- 提交后等用户确认或显式 `proceed`

### 3. 执行态（Execution）
连续循环：read plan → read 相关 SOP → 执行 → 调 `plan_mark(idx, status, note)` 标记 → 回到顶部，直到所有 `[ ]` 清空。
- 完成一步立刻 `plan_mark(idx, "done", "<一句话结果>")`
- 遇到 `[D]` / `[P]` 需要 dispatch subagent 或 Map 后再继续
- 遇到 `[?]` 评估条件，把分支结果写到 `note`，然后继续

### 4. 验证态（Verification）
执行完后调 `subagent_dispatch("verifier", ...)`，verifier 必须返回 `VERDICT: PASS | FAIL | PARTIAL`：
- **PASS**：所有交付物有效，进入收尾
- **FAIL**：critical 失败 — 在 plan.md 里插入若干 `[FIX]` 步骤后回到执行态；**最多 2 轮**
- **PARTIAL**：部分通过 — 主 agent 自己判断接受 or 修复

### 5. 收尾（Completion）
- 只有 verify=PASS 后才能把 `[VERIFY]` 步骤标 `[✓]`
- 在 `~/.gars/memory/global_mem.txt` 末尾追加这次任务的一行总结（如果有可复用经验）
- 写 `_keyinfo` 给可能的父 agent

## 重写计划

允许重写 plan，但每次重写必须在 `note:` 字段写明原因（"verify FAIL 第 1 轮，插入 3 个 [FIX] 步"）。

## 反模式

- ✗ 不要写"step 1: 思考一下" 这类没有可观测输出的步骤
- ✗ 不要在执行态阶段一次性把 5 步全标 `[✓]`，必须一步一标
- ✗ 不要跳过 verify 阶段直接报 done
