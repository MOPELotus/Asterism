import { randomUUID } from "node:crypto"

import plugin from "../../../lib/plugins/plugin.js"

import { AsterismApiError, AsterismClient } from "../model/client.js"
import { readConfig, validateConfig } from "../model/config.js"
import { executionBlockReason, recommendedExecutionCapabilities } from "../model/policy.js"

const PROVIDER_NAMES = { chaoxing: "学习通", welearn: "WELearn", uai: "UAI", cidaren: "词达人" }
const REMOTE_STATE = { pending: "未完成", in_progress: "进行中", completed: "已完成", expired: "已结束", removed: "已移除", unknown: "未知", not_open: "未开放" }
const taskSelections = new Map()

export class AsterismPlugin extends plugin {
  constructor() {
    super({
      name: "Asterism",
      dsc: "Asterism 课程、任务与执行控制面",
      event: "message",
      priority: 5000,
      rule: [
        { reg: "^#?(?:星芒|Asterism)(?:帮助)?$", fnc: "help" },
        { reg: "^#?(?:星芒|Asterism)状态$", fnc: "status" },
        { reg: "^#?(?:星芒|Asterism)账号$", fnc: "accounts" },
        { reg: "^#?(?:星芒|Asterism)课程(?:\\s+.*)?$", fnc: "courses" },
        { reg: "^#?(?:星芒|Asterism)任务(?:\\s+.*)?$", fnc: "tasks" },
        { reg: "^#?(?:星芒|Asterism)扫描\\s+\\S+$", fnc: "scan" },
        { reg: "^#?(?:星芒|Asterism)执行\\s+\\S+$", fnc: "prepareExecution" },
      ],
    })
    try {
      this.config = readConfig()
      validateConfig(this.config)
      this.gatewayClient = new AsterismClient(this.config)
      this.userSessions = new Map()
      this.notificationTimer = setInterval(() => void this.deliverNotifications(), this.config.notificationIntervalMs)
      this.notificationTimer.unref?.()
    } catch (error) {
      this.configurationError = error instanceof Error ? error.message : String(error)
    }
  }

  async help(e) {
    if (!(await this.authorize(e))) return true
    await e.reply([
      "Asterism 命令",
      "#星芒状态",
      "#星芒账号",
      "#星芒课程 [平台或平台:账号序号]",
      "#星芒任务 <平台或平台:账号序号> [未完成|全部]",
      "#星芒扫描 <平台或平台:账号序号>",
      "#星芒执行 <任务序号或完整 ID>",
      "普通且无需人工内容的任务会直接创建 Job；独立作业、考试和需要补内容的任务会给出 WebUI 确认链接；绑定 QQ 的任务会在群内 @ 通知完成或失败。",
    ].join("\n"))
    return true
  }

  async status(e) {
    return this.run(e, async (_client, identity) => {
      const health = await this.gatewayClient.health()
      await this.replyAt(e, `Asterism：${health.status || "ok"}\n账号：${identity.username}（QQ ${identity.qq}）`)
    })
  }

  async accounts(e) {
    return this.run(e, async (client) => {
      const payload = await client.accounts()
      const seen = new Map()
      const lines = (payload.items || []).map((account, index) => {
        const providerIndex = (seen.get(account.provider_id) || 0) + 1
        seen.set(account.provider_id, providerIndex)
        return `${index + 1}. [${account.provider_id}:${providerIndex}] ${providerName(account.provider_id)} · ${account.display_name} · ${authState(account.auth_state)}`
      })
      await this.replyAt(e, lines.length ? `平台账号（${payload.total}）\n${lines.join("\n")}` : "还没有平台账号，请先在 WebUI 添加并登录。")
    })
  }

  async courses(e) {
    return this.run(e, async (client) => {
      const selector = parseAccountSelector(argumentAfter(e.msg, /(?:星芒|Asterism)课程/i), false)
      const accounts = await client.accounts()
      const selected = selector ? [selectAccount(accounts.items, selector)] : accounts.items
      if (!selected.length) throw new UserFacingError("还没有平台账号")
      const pages = await Promise.all(selected.map(async (account) => ({ account, page: await client.courses(account.id) })))
      const lines = []
      for (const { account, page } of pages) {
        for (const course of page.items || []) lines.push(`${providerName(account.provider_id)} · ${truncate(course.title, 40)}`)
      }
      await this.replyAt(e, lines.length ? `课程（${lines.length}）\n${lines.slice(0, 30).map((line, i) => `${i + 1}. ${line}`).join("\n")}${lines.length > 30 ? `\n…另有 ${lines.length - 30} 门，请在 WebUI 查看` : ""}` : "当前没有课程。")
    })
  }

  async tasks(e) {
    return this.run(e, async (client) => {
      const args = argumentAfter(e.msg, /(?:星芒|Asterism)任务/i).split(/\s+/).filter(Boolean)
      const selector = parseAccountSelector(args[0], true)
      const showAll = args.includes("全部")
      const accounts = await client.accounts()
      const account = selectAccount(accounts.items, selector)
      const { tasks, total } = await loadProviderTasks(client, account.id, showAll)
      taskSelections.set(senderKey(e), { at: Date.now(), tasks })
      if (!tasks.length) {
        await this.replyAt(e, `${providerName(selector.provider)} 当前没有${showAll ? "" : "未完成"}任务。`)
        return
      }
      const lines = tasks.map((task, index) => `${index + 1}. [${REMOTE_STATE[task.remote_state] || task.remote_state}] ${truncate(task.title, 44)}`)
      const suffix = total > tasks.length ? `\n本次显示 ${tasks.length}/${total}；完整列表见 WebUI。` : ""
      await this.replyAt(e, `${providerName(selector.provider)}任务\n${lines.join("\n")}${suffix}`)
    })
  }

  async scan(e) {
    return this.run(e, async (client) => {
      const selector = parseAccountSelector(argumentAfter(e.msg, /(?:星芒|Asterism)扫描/i), true)
      const accounts = await client.accounts()
      const account = selectAccount(accounts.items, selector)
      await this.replyAt(e, `开始扫描 ${providerName(selector.provider)}；课程较多时会等待较久。`)
      const report = await client.scan(account.id)
      await this.replyAt(e, `扫描完成：课程新增 ${report.courses_created ?? 0}、更新 ${report.courses_updated ?? 0}；任务新增 ${report.tasks_created ?? 0}、更新 ${report.tasks_updated ?? 0}、未变化 ${report.tasks_unchanged ?? 0}。`)
    })
  }

  async prepareExecution(e) {
    return this.run(e, async (client) => {
      const selector = argumentAfter(e.msg, /(?:星芒|Asterism)执行/i)
      const taskId = resolveTaskId(e, selector)
      const task = await client.task(taskId)
      const reason = executionBlockReason(task)
      if (reason) {
        const identity = await this.gatewayClient.assertQq(e.user_id, true, `/tasks/${task.id}`)
        throw new UserFacingError(`${reason}：${this.config.webUrl}${identity.web_login_path}`)
      }
      const capabilities = recommendedExecutionCapabilities(task.capabilities || [])
      const result = await client.execute(task.id, capabilities, randomUUID())
      await this.replyAt(e, `已创建 Job：${result.execution.id}\n状态：${result.execution.state}\n${this.config.webUrl}/executions/${result.execution.id}`)
    })
  }

  async authorize(e) {
    if (this.configurationError) {
      await e.reply(`Asterism 插件未配置：${this.configurationError}`)
      return false
    }
    if (!e.isGroup) {
      await e.reply("Asterism 只在群聊中处理命令，请在已配置的群内使用并 @ 机器人。")
      return false
    }
    if (this.config.allowedGroups.size && !this.config.allowedGroups.has(String(e.group_id))) return false
    return true
  }

  async deliverNotifications() {
    if (!this.gatewayClient || !this.config.notificationGroups.size || !globalThis.Bot) return
    try {
      const response = await this.gatewayClient.claimQqNotifications()
      const reports = []
      for (const item of response.items || []) {
        let delivered = false
        let error = "notification_groups_unavailable"
        for (const groupId of this.config.notificationGroups) {
          try {
            const group = globalThis.Bot.pickGroup?.(groupId)
            if (!group?.sendMsg) continue
            const at = globalThis.segment?.at ? globalThis.segment.at(item.qq) : `@${item.qq}`
            await group.sendMsg([at, `\n${item.message}\n${this.config.webUrl}${item.web_login_path}`])
            delivered = true
            error = undefined
            break
          } catch (sendError) {
            error = sendError instanceof Error ? sendError.message : String(sendError)
          }
        }
        reports.push({ id: item.id, delivered, ...(error ? { error } : {}) })
      }
      if (reports.length) await this.gatewayClient.reportQqNotifications(reports)
    } catch (error) {
      logger.warn?.(`[Asterism] notification poll failed: ${error?.message || error}`)
    }
  }

  async run(e, action) {
    if (!(await this.authorize(e))) return true
    try {
      const asserted = await this.userClient(e.user_id)
      await action(asserted.client, asserted.identity)
    } catch (error) {
      if (error instanceof UserFacingError) await this.replyAt(e, error.message)
      else if (error instanceof AsterismApiError) await this.replyAt(e, `Asterism 请求失败：${error.code}${error.requestId ? `\n请求 ID：${error.requestId}` : ""}`)
      else {
        logger.error(`[Asterism] ${error?.stack || error}`)
        await this.replyAt(e, "Asterism 插件发生内部错误，请查看机器人日志。")
      }
    }
    return true
  }

  async userClient(qq) {
    const key = String(qq)
    const cached = this.userSessions.get(key)
    if (cached && cached.expiresAt - Date.now() > 60_000) return cached
    const identity = await this.gatewayClient.assertQq(key)
    const value = {
      identity,
      client: new AsterismClient({ ...this.config, token: identity.access_token }),
      expiresAt: Date.parse(identity.expires_at),
    }
    this.userSessions.set(key, value)
    return value
  }

  async replyAt(e, message) {
    const at = globalThis.segment?.at ? globalThis.segment.at(e.user_id) : `@${e.user_id}`
    await e.reply([at, `\n${message}`])
  }
}

class UserFacingError extends Error {}

function senderKey(e) {
  return String(e.user_id)
}

function argumentAfter(message, prefix) {
  return String(message || "").replace(/^#?/, "").replace(prefix, "").trim()
}

function parseAccountSelector(value, required) {
  const input = String(value || "").trim().toLowerCase()
  if (!input && !required) return undefined
  const match = /^(chaoxing|welearn|uai|cidaren)(?::([1-9]\d*))?$/.exec(input)
  if (!match) throw new UserFacingError("账号必须写成平台或平台:序号，例如 chaoxing:1")
  return { provider: match[1], index: match[2] ? Number(match[2]) : undefined }
}

function providerName(provider) {
  return PROVIDER_NAMES[provider] || provider
}

function authState(value) {
  const state = typeof value === "string" ? value : value?.state
  return { authenticated: "已认证", expired: "已过期", invalid: "失效", unauthenticated: "未认证" }[state] || state || "未知"
}

function selectAccount(accounts, selector) {
  const matches = accounts.filter((account) => account.provider_id === selector.provider)
  if (!matches.length) throw new UserFacingError(`没有 ${providerName(selector.provider)} 账号`)
  if (selector.index == null && matches.length > 1) throw new UserFacingError(`${providerName(selector.provider)} 有 ${matches.length} 个账号，请使用 ${selector.provider}:1 这样的序号选择。`)
  const account = matches[(selector.index || 1) - 1]
  if (!account) throw new UserFacingError(`${providerName(selector.provider)} 没有第 ${selector.index} 个账号`)
  return account
}

function resolveTaskId(e, selector) {
  if (/^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(selector)) return selector
  if (!/^\d{1,2}$/.test(selector)) throw new UserFacingError("请使用刚才任务列表中的序号或完整任务 ID")
  const selection = taskSelections.get(senderKey(e))
  if (!selection || Date.now() - selection.at > 10 * 60_000) throw new UserFacingError("任务列表已过期，请先重新查询任务。")
  const task = selection.tasks[Number(selector) - 1]
  if (!task) throw new UserFacingError("任务序号不存在。")
  return task.id
}

function truncate(value, limit) {
  const text = String(value || "")
  return text.length > limit ? `${text.slice(0, limit - 1)}…` : text
}

async function loadProviderTasks(client, accountId, showAll) {
  const tasks = []
  let offset = 0
  let total = 0
  do {
    const page = await client.tasks({ accountId, limit: 200, offset })
    total = page.total || 0
    for (const task of page.items || []) {
      if (showAll || !["completed", "removed"].includes(task.remote_state)) tasks.push(task)
      if (tasks.length >= 30) return { tasks, total }
    }
    offset += page.items?.length || 0
    if (!page.items?.length) break
  } while (offset < total)
  return { tasks, total }
}
