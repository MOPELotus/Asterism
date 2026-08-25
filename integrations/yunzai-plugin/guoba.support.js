import { readConfig, writeConfig } from "./model/config.js"

export function supportGuoba() {
  return {
    pluginInfo: {
      name: "asterism-plugin",
      title: "Asterism",
      author: "Asterism contributors",
      link: "https://github.com/Lotus-Asterism/Asterism",
      isV3: true,
      isV2: false,
      description: "Asterism 统一课程、任务与执行控制面",
      icon: "mdi:star-four-points",
      iconColor: "#7c3aed",
    },
    configInfo: {
      schemas: [
        { component: "Divider", label: "Asterism 服务" },
        { field: "apiUrl", label: "API 地址", component: "Input", required: true,
          bottomHelpMessage: "例如 http://127.0.0.1:8068" },
        { field: "webUrl", label: "WebUI 地址", component: "Input", required: true,
          bottomHelpMessage: "群内人工确认链接使用的、用户可访问的地址" },
        { field: "token", label: "机器人服务令牌", component: "InputPassword", required: true,
          bottomHelpMessage: "使用具备 qq_identity_assert 权限的系统 Service Token" },
        { field: "allowedGroups", label: "允许的群", component: "InputTextArea",
          bottomHelpMessage: "每行或逗号分隔一个群号；留空表示所有群" },
        { field: "adminContact", label: "管理员联系方式", component: "Input",
          bottomHelpMessage: "余额不足、充值或人工处理时展示" },
        { field: "requestTimeoutMs", label: "请求超时（毫秒）", component: "InputNumber",
          componentProps: { min: 1000, max: 600000, step: 1000 } },
      ],
      async getConfigData() {
        const config = readConfig({})
        return { ...config, allowedGroups: [...config.allowedGroups].join("\n") }
      },
      async setConfigData(data, { Result }) {
        try {
          writeConfig({ ...readConfig({}), ...data })
          return Result.ok({}, "Asterism 配置已保存，重载插件后生效")
        } catch (error) {
          return Result.error(error instanceof Error ? error.message : String(error))
        }
      },
    },
  }
}
