# Device / UX matrix

核心原则：Asterism 后端不需要知道用户具体是什么设备；所有入口只是把同一个 OAuth URL 送进一个能完成微信 OAuth 的环境，最后把 callback URL 送回 `SubmitCallback`。

| 设备情况 | 建议 UX | 状态 |
|---|---|---|
| 仅 1 台 Windows | WebUI 复制 OAuth URL -> Windows 微信文件传输助手 -> 桌面微信内打开/授权 -> 复制 callback -> 粘回 WebUI | **现场实测 PASS** |
| 仅 1 台 Android | 微信内打开 Asterism WebUI -> 当前页 OAuth -> callback 空白页 -> 复制链接 -> Android 返回 -> 粘贴 | **OAuth/callback 行为现场实测 PASS** |
| Windows + Android | Windows WebUI 显示二维码 -> Android 微信扫码授权 -> callback 复制后发机器人/手机 WebUI/电脑 WebUI | 推荐 |
| 2 台 Windows | A 显示/复制 OAuth；B 桌面微信完成 OAuth并复制 callback | 桌面微信能力已验证 |
| 2 台 Android | 单机即可；也可以 A 显示 QR、B 微信扫码 | 可用 |
| iPhone | 理论同 Android 的“微信内当前页授权 + callback 复制”，但本轮未现场验证 | 待测 |
| Mac + iPhone | 理论同 Windows + Android，但桌面 Mac 微信/手机 iOS 行为未现场验证 | 待测 |
| 仅 1 台 Mac | 取决于 macOS 微信是否与 Windows 微信一样能在内置浏览环境完成 OAuth并复制 callback | 待测 |

## Android Web probe result

- `iframe`：不能完成 OAuth。
- `window.open(OAuth)`：能授权，但表现为实际导航/历史切换；callback 后必须 Android 返回键回我们的页面。
- `target=_blank`：同上。
- `same-tab`：能授权。
- 先创建同源空白 child 再异步导航 OAuth：卡在“正在进入微信授权”。

结论：不要继续依赖 popup/iframe/window.name 去做纯 Web 自动 callback relay。

## Suggested WebUI

```text
词达人登录

[ 在当前微信中授权 ]

[ 显示二维码 ]
使用其他设备上的微信扫码

[ 复制授权链接 ]
可发送到桌面微信 / 文件传输助手

-----------------------------

授权完成后
[ 粘贴回调链接 ]

也可以直接把回调链接发送给 Asterism 机器人
```

PC 可通过 SSE/WebSocket 观察 pending login 状态；callback 若从机器人或另一端 WebUI提交，当前页面自动更新为成功。
