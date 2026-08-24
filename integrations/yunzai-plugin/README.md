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

先在 Asterism WebUI 创建 owner-bound Service Token，建议仅授予：

```text
provider_read, provider_manage, task_read, task_execute, task_command_proxy
```

把以下环境变量注入 Yunzai 进程；不要把 token 写进插件仓库：

```text
ASTERISM_URL=http://127.0.0.1:8068
ASTERISM_WEB_URL=https://asterism.example.com
ASTERISM_TOKEN=ast_st_...
ASTERISM_ALLOWED_QQ=你的QQ号[,另一个QQ号]
ASTERISM_ALLOW_GROUPS=false
ASTERISM_REQUEST_TIMEOUT_MS=180000
```

默认只允许私聊，且 `ASTERISM_ALLOWED_QQ` 为空时插件拒绝所有请求。一个插件实例使用一个
owner-bound token；多租户机器人应运行独立实例，后续再根据真实需求增加中心化 QQ 绑定。

## 命令

```text
#星芒
#星芒状态
#星芒账号
#星芒课程 [平台或平台:账号序号]
#星芒任务 <平台或平台:账号序号> [未完成|全部]
#星芒扫描 <平台或平台:账号序号>
#星芒执行 <刚才任务列表中的序号或完整任务 ID>
#星芒确认 <六位确认码>
```

普通资源执行需要二次确认，并在确认前重新读取任务。正式测评、答题 Draft、讨论文字、上传
和口语输入不会在 QQ 中降级成自由文本提交，而会返回对应 WebUI 页面。
同一平台有多个账号时，先用 `#星芒账号` 查看诸如 `chaoxing:1` 的稳定会话内选择格式；
未指定序号时插件会拒绝猜测账号。

## 自检

```bash
npm run check
npm test
```
