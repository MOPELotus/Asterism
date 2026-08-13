# Codex handoff — integrate Cidaren WeChat OAuth V2 into Asterism

不要从零重新逆向；本包中的当前协议已经实测闭环。

## 目标

在 Asterism 中正式落地 Cidaren 的 `AssistedBootstrap / WeChatOAuthV2`：

1. 创建 pending login；
2. 后端生成随机 OAuth `state` 与随机 `authorize_marker`（marker 必须不等于字符串 `2`）；
3. 生成微信 OAuth URL；
4. WebUI 可显示二维码、复制链接、或在当前微信环境导航；
5. 用户完成授权后，把 `app.vocabgo.com` callback URL 粘贴到 WebUI，或发给机器人；
6. 后端解析并严格校验 callback；
7. 使用 `code` 调当前 `/Wechat/V2/LoginByWechatCode`；
8. ECDH P-256 + HKDF-SHA256 + AES-GCM 解密 session；
9. 保存 UserToken/session 到 Asterism 的 credential store；
10. 用 `Student/Main` 验证；
11. pending login 标记 consumed；WebUI/机器人同步显示成功。

## 不要做

- 不要 MITM、不要架代理、不要依赖浏览器抓包。
- 不要恢复旧 `/Thirdpart/Authorize` 作为主路径；现场已经失败。
- 不要固定 `authorize=9`。
- 不要固定 `state=ASTERISM`。
- 不要把完整 `code` / `UserToken` / session 打进日志。
- 不要把 `state` 当 OAuth code；它只是 Asterism pending-login 关联与 CSRF nonce。
- 不要假设纯 Web 能自动读取跨域 callback；Android 微信探针已经排除 iframe/popup 这条正常 Web 路径。

## 建议数据模型

```text
PendingCidarenLogin
- id: opaque internal id
- owner_id
- oauth_state_hash              # 推荐只持久化 hash
- authorize_marker_hash         # 推荐只持久化 hash
- created_at
- expires_at                    # 例如 5 min
- consumed_at: optional
- status: pending | completing | succeeded | failed | expired
- failure_reason: optional/redacted
```

内存/短期状态可持有原始 `oauth_state` 与 `authorize_marker`，但不要写普通日志。

## 建议 API

名称按现有 Asterism 风格调整，不要为了对齐这里硬造新层：

```text
POST /.../cidaren/bootstrap
-> login_id, oauth_url, expires_at

GET /.../cidaren/bootstrap/{login_id}
-> pending/succeeded/failed/expired

POST /.../cidaren/bootstrap/{login_id}/callback
body: { callback_url }
-> validate + consume + V2 login
```

机器人入口可识别 `https://app.vocabgo.com/student/...` callback，抽取 `state` 后查找 owner 的 pending login，再复用同一个 service 方法。

## OAuth URL

```text
https://open.weixin.qq.com/connect/oauth2/authorize
  ?appid=wx2a694105a6abbe6d
  &redirect_uri=<urlencode(https://app.vocabgo.com/student/?authorize=<RANDOM_MARKER>)>
  &response_type=code
  &scope=snsapi_userinfo
  &state=<RANDOM_CSPRNG_NONCE>
  #wechat_redirect
```

`authorize_marker` 只需是不可预测的 opaque 值并且 `!= "2"`。官方当前前端仅对 `authorize === "2"` 自动执行 V2 登录；随机其他值会让 callback 保留下来供用户复制。

## Callback 严格校验

至少：

```text
scheme == https
host == app.vocabgo.com
path == /student/   # 可按真实路由做保守兼容，但不要接受任意 host
code 非空
state 精确匹配 pending login
random authorize marker 精确匹配 pending login
pending login 未过期
pending login 未 consumed
owner/session 匹配
```

先把状态原子地从 `pending -> completing`，防止 callback/code 并发重放；V2 成功后 `succeeded`，失败按可重试性决定是否允许重新创建 pending login。OAuth code 按一次性处理。

## 当前 V2 协议

详见 `docs/protocol.md` 和 `reference/cidaren_v2_reference.py`。实现时优先按仓库现有 Rust HTTP/crypto/domain 抽象重写，不要在生产路径 shell out Python。

## Asterism auth 分类

建议保持：

```text
Cidaren
├─ ImportedToken
│   └─ existing UserToken
└─ AssistedBootstrap
    └─ WeChatOAuthV2
        ├─ create pending login + OAuth URL
        ├─ human WeChat authorization
        ├─ submit callback URL
        ├─ V2 crypto bootstrap
        ├─ save UserToken/session
        └─ Student/Main validate
```

## 测试要求

至少增加：

- OAuth URL 生成：state/marker 每次不同，marker 永不为 `2`；
- callback parser：query + SPA/hash 兼容，但 host/path 必须严格；
- state mismatch / marker mismatch / expired / consumed / wrong host 拒绝；
- pending login 并发 consume 原子性；
- V2 crypto fixture：P-256 SPKI Base64、HKDF info、AAD、AES-GCM；
- 日志 redaction；
- live test 必须显式 opt-in，不进入默认 CI。

## 已验证设备 UX

- 单 Windows + Windows 桌面微信：PASS。
- Android 微信内 WebUI：OAuth PASS；callback 需复制链接并返回。
- Windows + Android：PC QR + 手机微信授权 + callback 回传可用。
- macOS/iOS 尚未现场实测；不要在文档里写成 confirmed。
