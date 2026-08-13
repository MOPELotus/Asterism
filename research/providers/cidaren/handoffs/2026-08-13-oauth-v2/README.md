# Cidaren / 词达人 OAuth V2 clean bootstrap handoff

日期：2026-08-13

这是给 Asterism / Codex 的完整技术交接包。目标是：**不使用 MITM / 代理抓包，在用户主动完成微信授权后，使用当前词达人 V2 登录协议取得并验证自己的 UserToken/session。**

## 已实测结论

1. 当前前端版本：`2.7.0.260715_01`。
2. 微信 AppID：`wx2a694105a6abbe6d`。
3. 当前官方 OAuth `/authorize` 懒加载 chunk 4398 只是 OAuth 跳转器，不存在 PC 扫码轮询/回传桥。
4. 官方 OAuth 回调 `authorize=2` 会被当前前端自动消费并走 `/Wechat/V2/LoginByWechatCode`。
5. 为了让用户手动拿到 callback，可以使用**每次随机、且不等于 `2`** 的 `authorize` 标记；`state` 也必须每次用 CSPRNG 随机生成，不使用固定项目名。
6. 用户自己的账号已实测：OAuth -> callback code -> V2 ECDH/HKDF/AES-GCM -> UserToken/session -> `Student/Main` 验证，整条链 PASS。
7. Android 微信内 WebUI：除 iframe 外，普通导航 / window.open / target=_blank 均能正常完成 OAuth；最终 callback 落在词达人页面，需要用户复制链接并返回 WebUI。纯 Web 无正常机制自动跨域取回 callback URL。
8. **单台 Windows + Windows 桌面微信已实测完整闭环 PASS**：复制 OAuth URL到文件传输助手 -> 桌面微信内打开并授权 -> callback -> 复制当前链接 -> 粘回 WebUI；host / authorize / state / code 全部匹配。
9. 旧 `/Thirdpart/Authorize` 路线现场测试已失败（微信认证错误），不再作为主路线。

## 建议的正式产品形态

Asterism 统一实现一个 `PendingCidarenLogin`，向用户暴露三种只是“传递同一 OAuth URL”的入口：

- 在当前微信环境中打开；
- 显示二维码，供其他微信设备扫码；
- 复制授权链接，发送到桌面微信/其他微信环境。

授权完成后统一走 `SubmitCallback`：用户将 callback URL 粘贴到 WebUI，或直接发给 Asterism 机器人。后端校验 `host + state + authorize + code + TTL + one-time`，随后完成 V2 登录。

## 目录

- `CODEX_HANDOFF.md`：直接交给 Codex 的实现指令。
- `docs/protocol.md`：当前协议与 crypto 精确说明。
- `docs/ux-device-matrix.md`：设备组合与 UX。
- `docs/security.md`：pending login、日志与凭据安全。
- `reference/cidaren_v2_reference.py`：Python 参考实现（研究/互操作参考，不应原样作为生产凭据存储层）。
- `reference/requirements.txt`：参考实现依赖。
- `probes/`：本轮实际用过的 Web/桌面微信探针。
- `evidence/`：当前前端静态分析与运行抓取报告。

## 重要约束

- 正式实现不要使用固定 `authorize=9`。
- 正式实现不要使用 `state=ASTERISM` 或任何固定项目指纹。
- `authorize` 与 `state` 都由后端 CSPRNG 生成；`authorize != "2"`。
- OAuth `code` 是一次性临时登录凭据；完整 code/token/session 不得进普通日志。
- 不要提交 `UserToken`、session JSON、OAuth callback 到 Git。
- 这套路径的设计用途是用户自己的账号/明确授权的账号登录，不做静默凭据截取。
