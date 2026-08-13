# Evidence index

## `evidence/cidaren-authorize-4398-analysis.txt`

当前 `/authorize` lazy chunk 4398。关键事实：组件构造 `/?authorize=2`，然后直接导航到微信 OAuth；state 来自 `deviceCode`；无 polling/QR bridge。

## `evidence/cidaren-crypto-analysis.txt`

当前 app bundle crypto/signing 深挖。关键事实：

- version `2.7.0.260715_01`
- sign suffix
- P-256 SPKI Base64
- `v1`
- `vcg-auth`
- `vcg-auth-aes`
- AES-GCM 128-bit tag
- AAD 生成/校验
- V2 login decrypt helper

## `evidence/report.txt`

较早一轮当前前端静态提取，包含 API defs、V2 callsite、headers/interceptors 等。

## `evidence/1-report.txt`

Playwright 页面/资源探针报告，确认 `/authorize` lazy chunk 4398 被请求；同时保存大段当前 app bundle 上下文。

## `probes/*.html`

本轮真实设备行为探针：

- `oauth-window-probe.html`：Android 微信中 window.open/blank/iframe/same-tab 初测。
- `wechat-oauth-window-probe-v2.html`：Android 微信 popup/browsing-context 细分测试。
- `desktop-wechat-oauth-probe.html`：单 Windows + Windows 微信完整 OAuth/callback 闭环测试。

注意：探针只生成随机 marker/state，不包含用户这次实际 callback code/token/session。
