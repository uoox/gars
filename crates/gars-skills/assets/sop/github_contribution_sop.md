---
key: github_contribution_sop
name: GitHub Contribution SOP
category: workflow
tags: [github, pr, contribution]
form: markdown
autonomous_safe: false
one_line_summary: 给外部仓库提 PR 的标准动作：读规约 → 跑测试 → 小步 commit → fork+PR。
---

# GitHub Contribution SOP

## 前置必做

1. 读 `CONTRIBUTING.md`、`CODE_OF_CONDUCT.md` 全文。如无，看 README 末尾。
2. 找已有 issue；如无，先开 issue 描述问题再做。
3. 找现有测试：`find . -name "test_*.py" -o -name "*_test.go" -o -name "*.test.ts" | head`。

## 分支与提交

- 不在 main 上工作。`git checkout -b fix/<short-description>`。
- **每个 commit 单职责**，能通过 `cargo test` / `pytest` / 等价测试。
- Commit 信息用 conventional：`fix(scope): ...` / `feat(scope): ...` / `docs: ...`。
- 不混入无关重排、tab/space 转换、自动 format 改动。

## 测试

- 修了功能就加测试。无测试的 PR 通常拒收。
- 跑全套测试再 push：`make test` / `npm test` / `cargo test`。
- 跑 lint：`make lint` / `npm run lint` / `cargo clippy -- -D warnings`。

## 提 PR

- 标题简洁、动词开头：`Fix race in tokenizer when stream ends mid-token`。
- 描述结构：
  - **What**：一句话讲改了啥
  - **Why**：动机/issue 链接
  - **How**：关键设计点
  - **Test plan**：跑了什么命令，输出关键截图
- 第一次提交：先 fork → 自己仓库 push → 跨 fork 开 PR。
- 提交前 rebase 到上游 main：`git fetch upstream && git rebase upstream/main`。

## 沟通

- 评审反馈 24h 内回复（哪怕只是"在看"）。
- 不为反对而反对；维护者权重 > 你的偏好。
- 长时间停滞的 PR 主动 ping，但不要 spam。

## 大小

- 改动 > 400 行的 PR 应该考虑拆。
- 大重构先开 issue 谈方案、不直接发巨型 PR。
