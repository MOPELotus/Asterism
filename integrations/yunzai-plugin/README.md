# Asterism Plugin for Yunzai

这是 Asterism `0.0.1` 的薄 QQ 控制面，兼容 Miao-Yunzai 与 TRSS-Yunzai。插件只调用
Asterism HTTP API，不包含任何 Provider 协议、凭据刷新或任务执行实现。

## 安装

将本目录复制到 Yunzai 的 `plugins/asterism-plugin`：

```bash
cp -a /path/to/Asterism/integrations/yunzai-plugin /path/to/Miao-Yunzai/plugins/asterism-plugin
```

Windows PowerShell：

```powershell
Copy-Item -Recurse `
  -Path C:\path\to\Asterism\integrations\yunzai-plugin `
  -Destination C:\path\to\Miao-Yunzai\plugins\asterism-plugin
```

插件没有第三方 npm 依赖。Node.js 18+ 自带的 `fetch` 即可运行。

## 配置

推荐在锅巴面板中配置。先在 Asterism 创建供机器人网关使用的系统 Service Token，只授予：

```text
provider_read
task_read
task_execute
task_command_proxy
qq_identity_assert
notification_delivery_report
```

把以下环境变量注入 Yunzai 进程；不要把 token 写进插件仓库：

```text
ASTERISM_URL=http://127.0.0.1:8068
ASTERISM_WEB_URL=https://asterism.example.com
ASTERISM_TOKEN=ast_st_...
ASTERISM_ALLOWED_GROUPS=允许使用的群号[,另一个群号]
ASTERISM_NOTIFICATION_GROUPS=接收待确认通知的群号[,另一个群号]
ASTERISM_NOTIFICATION_INTERVAL_MS=30000
ASTERISM_ADMIN_CONTACT=余额或人工处理的管理员联系方式
ASTERISM_REQUEST_TIMEOUT_MS=180000
```

插件仅响应群聊命令，不处理私聊。通知网关接口支持按固定间隔领取待确认通知，并在允许的群内 @ 对应 QQ；发送成功/失败
会回传 Asterism，失败通知会按服务端退避重试。通知分为截止前 `confirmation_due` 和截止后
`deadline_missed` 两类；后者只告知草稿保留，不会提供确认提交链接。当前插件已提供领取/回执客户端边界，定时群投递适配仍需在目标
Yunzai 实例确认其群消息 API 后启用。`ASTERISM_ALLOWED_GROUPS` 留空表示允许所有群。机器人会把
发送者 QQ 交给受信任的 Asterism 网关：已有绑定时使用对应用户；不存在时创建用户名等于 QQ
号的普通用户。Provider 账号、任务、余额和执行记录因此始终按真实 QQ 用户隔离。

如果 Yunzai 事件的 `e.isMaster` 为真，插件会在 QQ assertion 中携带受信任的 master
attestation。Asterism 只接受同时拥有 `qq_identity_assert` 和 `task_command_proxy` 的网关令牌，
并且提权是单向、可审计的；普通用户请求不能通过自行提交字段提升权限。master 代用户操作时，
Web API 使用 `X-Asterism-Target-Owner` 选择资源归属，操作者和资源 owner 会分别进入权限与审计边界。

## 命令

```text
#星芒
#星芒状态
#星芒账号
#星芒课程 [平台或平台:账号序号]
#星芒任务 <平台或平台:账号序号> [未完成|全部]
#星芒扫描 <平台或平台:账号序号>
#星芒执行 <刚才任务列表中的序号或完整任务 ID>
```

群主/管理员可以在命令末尾显式指定目标 QQ，例如
`#星芒任务 chaoxing:1 全部 用户:123456789` 或
`#星芒执行 3 目标:123456789`。插件只接受数字 QQ，服务端通过
`X-Asterism-Target-Owner` 绑定资源 owner；执行审计中的 actor 仍是网关 Service Token，
不会把目标用户的权限提升给普通群成员。非管理员带目标参数会直接拒绝。

普通且不需要人工内容的资源任务直接创建 Job。正式测评、答题补漏、讨论文字、上传和口语输入
不会在 QQ 中降级成自由文本提交，而会返回对应 WebUI 页面；独立作业和考试仍由用户在 WebUI
执行提交前确认。
同一平台有多个账号时，先用 `#星芒账号` 查看诸如 `chaoxing:1` 的稳定会话内选择格式；
未指定序号时插件会拒绝猜测账号。

## 自检

```bash
npm run check
npm test
```
