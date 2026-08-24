# UAI 第一条真实 Provider vertical slice

状态：只读 Adapter 已接通；真实账号确认执行链应按单元直接完成，不走题库逐题答题

目标分支：`0.0.1`

首选 donor：`create-try-now/AutoFinish_UxiaoyuanAI@bef0d29155ce`

运行环境：Python 3.10，`requests`、`pyjwt`、`pycryptodome`

许可证：Apache-2.0

## 为什么先做 UAI

仓库现有审计显示，该 donor 是一个接近可直接运行的 Python 单脚本，已经包含：

- 密码登录与 JWT 获取；
- 教程选择、课程结构和任务完成状态读取；
- 题目与加密标准答案读取；
- 单选、多选、填空、排序、改错、翻译、复合题等分支；
- 主观题、讨论、口语、上传和出口问卷处理；
- 原生提交体构造、频率控制、日志和未完成清单。

它同时具有清晰的 Apache-2.0 授权。与需要浏览器上下文、微信/XWeb Capture、多个
donor 拼接或许可证尚不明确的首条方案相比，它更适合先验证 `0.0.1` 的核心假设：
不重写平台协议，只包住一个已经能工作的执行器。

本选择基于以下仓库内资料：

- [`UPSTREAMS.md`](../../UPSTREAMS.md)；
- [`research/providers/uai/FULL_UPSTREAM_SWEEP.md`](../../research/providers/uai/FULL_UPSTREAM_SWEEP.md)；
- [`research/providers/uai/LICENSE_NOTES.md`](../../research/providers/uai/LICENSE_NOTES.md)；
- [`research/providers/uai/CAPABILITY_MAP.md`](../../research/providers/uai/CAPABILITY_MAP.md)。

开始编码前只需确认 donor revision、README 运行方法和依赖仍可用；不重新做一次完整
协议迁移审计。

## vertical slice 的单一目标

用一个已授权的真实 UAI 测试账号，从 Asterism UI 完成下面一条链：

```text
添加账号并登录
  -> 展示课程
  -> 展示课程内任务与完成状态
  -> 选择一个未完成 Unit / Task
  -> Worker 沿用 upstream 的类型分支和原有提交顺序直接完成
  -> 时长由独立 Worker 路径处理
  -> 重新读取完成状态
  -> Asterism Job/UI 展示进度、日志和最终结果
```

原计划曾要求把普通题目送入 Asterism 题库再逐题返回答案。真实账号验证后确认这不是
UAI 的产品执行边界：donor 已按 Unit 内容类型直接处理完成，Asterism 不应拆开它的内部
答题步骤。内容/答案解码只作为 Worker 私有实现和协议诊断；讨论、口语、上传、AI 生成、
复合内容仍由 donor 自己处理。页面停留/学习时长作为独立路径接入。

## 保持 upstream 原样的边界

以下逻辑由 donor 继续拥有，不移植到 Rust，也不在 Asterism 中重建：

- 登录请求、JWT 和 annotator token 处理；
- AES 解密；
- 课程结构遍历和完成判定；
- 题型识别与 upstream 的处理分支；
- submit body 的字段、排序、编码和协议版本；
- 请求顺序、节流、平台错误识别和完成状态重读。

Asterism Adapter 只允许做接入所需的最小改动：

- 把脚本头部硬编码配置改为运行时输入；
- 把交互式课程选择改为由 Asterism 传入稳定目标；
- 把 `print`/交互提示投影为结构化 log、progress、question、result、error 事件；
- 在只读诊断需要时保留 upstream 解出的内容 shape，但不建立答案交换钩子；
- 支持 Asterism 取消当前子进程；
- 保留精确 upstream revision、许可证和很小的 Asterism patch 记录。

不得借此重排请求、抽出通用协议库、重写加密/签名、把提交搬入 Rust，或先改造成
面向四个平台的 Worker SDK。

## 第一版运行形态

第一条链路默认采用“每个操作或 Job 启动一个 Python 子进程”，因为这最接近 donor
原始 CLI 环境，且不需要先维护常驻服务。

```text
WebUI / API
    |
Asterism 创建并持有 Job
    |
    +-- stdin: 账号引用、operation、目标和运行设置
    |
    `-- UAI Python Worker
          |
          +-- stdout: JSON Lines 结构化事件
          `-- stderr: 被 Adapter 收集并脱敏的原始诊断
```

这是 UAI vertical slice 的局部选择，不是其他 Worker 必须遵循的规范。若真实运行证明
常驻进程或 localhost HTTP 明显更简单，再在 UAI 内调整。

### 最小操作

首条链路只实现实际用到的操作：

| 操作 | 目的 |
|---|---|
| `health` | 检查 Python 与 donor 依赖是否可加载 |
| `authenticate` | 使用 Asterism 注入的账号材料执行 donor 登录并返回不透明 session |
| `courses` | 返回当前账号的课程公共字段和 native payload |
| `tasks` | 返回选定课程的任务与 upstream 完成状态 |
| `inspect` | Worker 私有诊断：读取 Task 内容 shape，不注册产品 Question capability |
| `run` | 沿用 donor 原分支直接完成同一 Task |
| `duration` | 独立处理 donor 的页面停留/时长逻辑 |

Worker 不另建 Job 数据库或 Job status API。Job 状态和日志由 Asterism 已有控制面保存；
取消由 Asterism 终止所持有的子进程并记录结果。

### 最小事件

JSON Lines 只需要覆盖 `log`、`progress`、`result` 和 `error`；`inspect` 可在显式诊断时
返回 Provider-private 内容 shape。每条事件携带 Asterism 生成的 Job correlation ID。字段只随真实链路增加，
第一版不承诺跨 Provider 稳定。

原始 donor 输出不得与协议行混在一起；Adapter 将其捕获为脱敏 log 事件。密码、JWT、
Cookie、annotator token、题目/答案原文和完整私有 payload 不写入普通日志。

## 控制面映射

### Account 与 session

Asterism 继续拥有 Provider Account、权限和 secret storage。Worker 仅在进程内使用
传入的秘密，并把更新后的 session 当作 UAI opaque payload 返回；控制面不解释或打印
JWT 内部字段。第一条链路不要求设计跨 Provider 的统一刷新协议。

### Course 与 Task

公共字段满足现有 UI 展示即可：provider、account、course、title、state、deadline 和
capabilities。UAI 的 course instance、publish version、Unit/Section/Micro/Group 层级、
tab/base 类型和完成标志保留在 UAI native payload，交还 Worker 时不得丢失。

### 内容、完成与时长

UAI 不注册产品级 QuestionInventory/QuestionParse。Worker 可以在诊断报告中记录
SingleChoice、MultipleChoice、Ordering、口语、讨论、上传等 native shape，但这些记录
不生成 QuestionSnapshot/AnswerCandidate，也不改变 upstream 的直接执行分支。

任务完成与时长保持两个 Provider-private 操作。Asterism 只选择目标、调度 Worker、展示
日志和重新读取完成状态，不在 Core 中重建 submit body 或逐题答案协议。

## 实施批次

### UAI-1：原 donor 冒烟

- 在独立 Python 环境按原 README 启动固定 revision；
- 使用授权测试账号验证登录、课程、结构和只读完成状态；
- 记录必要环境、依赖和平台当前阻断，不修改 Asterism；
- 不把真实凭据或响应提交到仓库。

### UAI-2：最薄 Adapter

- 保留 upstream 快照并分开存放 Adapter；
- 外部注入配置和目标；
- 增加子进程请求/事件边界；
- 先接 `health`、`authenticate`、`courses`、`tasks` 和 `inspect`；
- 让真实账号内容出现在现有 Course / Task / Question UI。

### UAI-3：直接执行与独立时长

- 将选定 Unit / Task 交回 Worker；
- Worker 保留 upstream 原有内容类型分支、提交顺序和节流；
- 时长使用单独 operation/Job，不与完成提交混成逐题状态机；
- Asterism 只保存进度、日志、结果和完成状态回读。

### UAI-4：第一次真实完成

- 由用户明确选择一个适合测试的未完成普通 Task；
- Asterism 创建 Job，Worker 执行 upstream 原有提交链；
- 流式保存 progress/log；
- 用 donor 原有完成状态读取重新确认结果；
- UI 展示成功、失败、取消或需人工处理的最终状态。

### UAI-5：根据证据修边界

第一次完整运行后才决定是否需要常驻 Worker、session 复用、更多事件字段、专用错误码
或新的 native renderer。没有在真实链路中出现的问题不进入该批次。

## 验收证据

| 用户结果 | 必须保留的证据 |
|---|---|
| 账号可用 | Asterism account 绑定成功；日志无秘密；Worker 登录结果可理解 |
| 课程可见 | UI 与 donor 对同一真实账号列出的目标课程一致 |
| 任务可见 | UI 展示选定课程的目标 Task、层级和新鲜完成状态 |
| 内容边界可信 | 内容 shape 只留在 Worker 私有诊断，不误注册为题库 Question |
| 执行目标可控 | 只有用户选择的 Unit / Task 交给 Worker；不写测试内容 |
| 任务已执行 | Job 事件能还原进度；提交由 donor 原 builder 发出 |
| 完成可信 | Worker 重新读取 donor 使用的完成标志，UI 显示最终状态 |
| 失败可处理 | 网络、认证、题型、平台拒绝和取消均成为明确 Job 结果，不假成功 |

离线测试可以覆盖 Adapter 编解码、事件解析、内容 shape 和日志脱敏，但不能代替这条真实
账号验收记录。真实验证记录只保存时间、donor revision、Asterism commit、脱敏目标
类别、结果和已知限制。

## 首条切片明确不做什么

- 不把 Python donor 翻译成 Rust；
- 不把 UAI submit body 移入 Core；
- 不让 UAI 适配完整的 SubmissionBuild / MutationReceipt / Recovery 链；
- 不先实现四 Provider 通用 SDK 或 schema；
- 不先接 AI、Whisper、上传、复合口语、讨论或浏览器停留自动化；
- 不删除仓库中现有 UAI Rust Provider；
- 不以旧 Rust Provider 的结构决定 Python Worker 的内部模块；
- 不在没有真实失败证据时增加复杂重试、恢复或数据库结构。

## 原完整 vertical slice 退出条件（当前不执行）

下面条件全部满足后，才把第一条链路标记为完成：

1. 授权真实账号从 Asterism 成功登录并看到课程/任务；
2. 至少一个真实普通 Task 由 Worker 沿用 upstream 原分支直接完成；
3. 独立时长路径可单独调度和观察，不依赖题库答案；
4. 新鲜状态读取确认该 Task 达成 donor 定义的完成状态；
5. WebUI 能看到 Job 进度、脱敏日志和最终状态；
6. 同一链路的失败与取消不会显示为成功；
7. upstream revision、许可证、依赖和最小 patch 均可追溯。

完成后先复盘这一条链路，再决定第二个平台和最小共享抽取；不在复盘前扩展终极架构。

## 当前实现进度

截至 2026-08-22，首个代码批次已经完成：

- `workers/uai/worker.py` 可直接加载外部固定 donor 入口，不复制或重写其协议逻辑；
- `SOURCE.json` 固定 donor revision、Apache-2.0 来源和入口 SHA-256，文件不匹配时拒绝启动；
- 已实现 `health`、`authenticate`、`courses`、`tasks` 和 `inspect` 的 Python Adapter
  操作，其中后四项等待授权真实账号验证；
- donor stdout/stderr 被转换为结构化事件，已知凭据在日志和意外 traceback 中脱敏；
- UAI 起步的 Rust 子进程客户端现只抽取了四个平台都需要的协议绑定、超时与输出限制；
  各 Provider 登录和扫描逻辑仍分别保留在自己的 Python Adapter；
- 受保护的 `GET /api/v1/providers/uai/worker/health` 已加入 API、OpenAPI 和生成式
  TypeScript client；
- 未配置 Worker 时该端点返回明确的 `503 uai_worker_not_configured`，不影响 Asterism
  总体健康；
- 隔离 Python 环境已成功加载真实固定 donor，实际 daemon/API 只读冒烟也返回相同
  revision 和可用操作。

真实账号已经验证登录、2 门课程和 558 个任务读取。100-task 诊断样本读取 246 个内容
节点且零失败，但根据真实产品执行方式，UAI 的 `questions` 不再注册到 Asterism 题库链。
直接完成、独立时长和新鲜完成状态回读仍在当前只读停止线之后。
