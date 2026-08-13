# Security / operational notes

## Pending login

- `state`：至少 128-bit CSPRNG，推荐 256-bit；一次性、短 TTL。
- `authorize_marker`：独立随机值，不能固定；必须 `!= "2"`。
- 如果持久化，优先保存 hash；原始值只在生成 OAuth URL和当前 pending context 中短期存在。
- callback consume 使用事务 / compare-and-swap，把 `pending -> completing` 做成原子操作，防止并发重放。
- OAuth code 按一次性凭据处理。

## Callback allowlist

不要只搜索字符串 `code=`。必须 URL parse 后验证：

```text
scheme=https
host=app.vocabgo.com
path in explicitly accepted student callback paths
state exact match
authorize exact match
code non-empty
not expired
not consumed
owner/session authorized
```

拒绝 userinfo 混淆、相似域名、任意 redirect、javascript/data/file scheme。

## Credentials

- UserToken/session 放现有 Asterism credential store。
- 不写 repo，不写 fixture，不写普通日志。
- 不把 callback URL原样写日志，因为它包含一次性 code。
- 日志仅输出 redacted：前 6 + 后 6 或 hash prefix。
- 错误对象/HTTP debug dump 也要经过 redactor。

推荐 `.gitignore`：

```gitignore
cidaren-token.txt
cidaren-login-session.json
*callback*.txt
*.credentials.json
```

## Live tests

- live OAuth / login 测试必须显式 opt-in。
- CI 默认只跑纯 fixture/crypto/parser 测试。
- live test 不上传 token/session artifact。

## Scope

此实现用于用户本人或明确授权账号的登录互操作。它不依赖 MITM、代理或静默拦截第三方凭据。
