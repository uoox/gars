---
key: keychain_sop
name: Keychain SOP
category: security
tags: [keychain, secrets, credentials]
form: markdown
autonomous_safe: true
one_line_summary: 用 ~/.gars/keychain.enc 存凭证；XOR 简单加密，禁止往明文 memory 写密钥。
---

# Keychain SOP

铁律
- API key / token / 密码必须走 keychain_set，不能写入 memory/*、不能写入日志。
- 工具结果中遇到密钥要遮蔽（首 6 + 末 6 + 长度）。

工具
- keychain_set(name, value | file_path)
- keychain_use(name)：返回明文（仅供子流程消费）
- keychain_list()：只返回掩码

文件
- ~/.gars/keychain.enc：XOR 密钥来源于 user+host+magic 的 sha256，便携性弱但够防偶然泄露。
