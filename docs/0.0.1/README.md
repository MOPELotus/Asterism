# Asterism 0.0.1 路线章程

状态：生效，适用于 `0.0.1` 分支

建立日期：2026-08-22

分支基线：`0.1.0` 的 `95875bd`（`fix(uai): accept numeric epoch shapes`）

## 分支边界

`0.0.1` 是一条独立的产品化路线。创建它时，`0.0.1` 与 `0.1.0`
共同指向 `95875bd`；后续 `0.0.1` 工作不得回写、重构或删除
`0.1.0` 与 `master` 上的既有实现。它们保留此前以 Rust、Thick Core、
统一 Capability 和 clean-room Provider 实现为中心的完整架构路线。

本目录是 `0.0.1` 的分支级实施依据。现有 `docs/architecture/`、
`research/providers/` 和 `UPSTREAMS.md` 继续提供历史设计、协议证据、donor
清单和审计资料，但不再自动构成 `0.0.1` 的实现门槛。发生冲突时按以下顺序
决策：

1. 真实可用性；
2. 当前 upstream 已验证行为；
3. 最小接入成本；
4. Asterism 用户体验一致性；
5. 长期架构文档。

路线建立时只新增文档；当前四个平台均已进入 upstream-backed Worker 执行接入，
WebUI 和真实账号复验是当前收口工作。

## 产品定义

`0.0.1` 的 Asterism 是统一控制面，不再要求自己重写每个平台的完整执行面。

```text
Asterism
  = WebUI
  + 用户、权限与 Provider Account
  + 课程/任务聚合视图
  + Question / Answer 体系
  + Scheduler
  + Job、日志与状态管理

Provider 实际执行
  = 经过真实用户验证的 upstream 原有逻辑
  + 尽可能薄的 Asterism Adapter
```

成功标准是用户能够在 Asterism 中添加真实账号、看到课程和任务、查看题目、
使用题库或审核答案、调度任务，并由对应 Worker 调用成熟 upstream 完成任务；
UI 同时显示进度、日志和最终状态。

## 保留的控制面

以下既有能力优先复用，不因路线变化而主动重写：

- WebUI、用户、权限、Service Token 和 Provider Account 管理；
- Course / Task 聚合、详情和状态展示；
- Question、QuestionSnapshot、AnswerCandidate、Answer History、本地/全局题库
  和人工审核；
- Execution / Job 的可见状态、事件和日志；
- Scheduler、定时执行、批量调度和多账号管理；
- 基本配置、HTTP API 和现有 OpenAPI/WebUI 调用链；
- 已有题目审核、下一题、任务详情等用户流程。

“复用”不等于要求 Worker 穿过现有全部 Core 抽象。已有抽象可以保留；只有当
真实接入自然需要时才使用。

## Provider 执行面原则

1. **先运行 upstream。** 先在其原始语言、依赖和调用顺序中复现已知可用链路。
2. **再加薄适配层。** Adapter 只处理 Asterism 调用、配置注入、事件输出、答案
   交换和结果映射。
3. **最后做必要修改。** 只有阻塞实际接入的问题才修改 upstream；HTTP、加密、
   签名、DOM、状态机和提交顺序默认保持原样。
4. **语言不是目标。** Python、Java、Rust、TypeScript/JavaScript、浏览器脚本、
   Playwright 或 Selenium 都可成为 Worker 的实际运行时。
5. **不同 Provider 可以不同。** 不先设计四个平台共用的终极 Worker SDK、IPC、
   状态机或数据模型。
6. **不为漂亮而重写。** 已经存在的登录、任务、答题、提交、口语、上传、讨论、
   签到、浏览器交互和特殊题型逻辑优先直接改造接入。

Worker 的最小职责是执行 upstream 并向 Asterism 报告事实。Asterism 的最小职责
是创建 Job、提供账号和目标、记录事件、保存可共享的题目/答案，并展示结果。

## 薄 Worker 边界

第一版只围绕真实 vertical slice 增加操作。候选操作包括：

- `health`；
- `authenticate`；
- `courses`；
- `tasks` / `task_detail`；
- `questions`；
- `run`；
- 运行中的 progress、log、question、result 和 error 事件。

Job 状态、调度和历史由 Asterism 管理。若一个 Worker 采用“每 Job 一个子进程”，
取消可以直接终止该子进程，不必先发明 Worker 内部 Job 服务。若另一个 upstream
天然适合常驻 HTTP、浏览器扩展或 Java 服务，可以使用不同边界。

只有第一条真实链路暴露出的共同需要，才进入下一版 contract。不同 Worker
出现相似字段，不足以单独证明需要共享框架。

## 数据与题库边界

Course、Task 和 Question 使用“公共字段 + Provider-private payload”：

- 公共 Task 优先保存 provider、account、course、title、state、deadline 和
  capabilities；
- 平台标识、层级、版本、路由参数和特殊状态可以留在不透明私有 payload；
- SingleChoice、MultipleChoice、TrueFalse、FillBlank、ShortAnswer 等自然映射
  到现有 Question / Answer 模型；
- Matching、Ordering、Composite、特殊口语、特殊 DOM 交互等不能自然映射的内容，
  可以保持 provider-native，并由相应 Worker 或专用 WebUI renderer 处理。

题库负责答案证据、选择、审核和复用，不接管 upstream 的提交协议：

```text
Worker 读取原始题目
  -> Asterism 保存快照并选择答案
  -> 答案返回 Worker
  -> Worker 继续使用 upstream 原有编码和提交逻辑
```

Provider-native 标准答案也应作为带来源的 AnswerCandidate 进入控制面；不得为了
统一题库而在 Asterism 中重新构造平台 submission wire。

## Scheduler 与批量执行

Scheduler 只决定何时运行、运行哪个账号和 Course / Task，并记录 Job 状态、日志
和结果。“如何完成”完全交给 Worker。

第一版批量执行可以循环创建多个普通 Worker Job。只有实际运行证明 parent/child
lifecycle、跨 Job 原子性或专用恢复是必要条件时，才扩展批量抽象。

## 现阶段非目标

除非真实运行明确需要，`0.0.1` 不主动继续强化：

- 全部 Provider 协议的 Rust 原生实现；
- 所有 Provider 共用的执行状态机、Capability 或 QuestionKind；
- 统一 SubmissionBuild、MutationIssue、MutationReceipt、Recovery、BrowserBridge
  或 BatchExecution 终态；
- crash consistency 极限强化、disaster recovery、HA 和高并发；
- Smart Scheduler、Analytics、Notifications；
- 为未来功能预建数据库结构；
- 理论上的 cross-provider consistency；
- 仅为删除既有复杂代码而进行的大重构。

这些既有代码继续保留；新的 Worker 链路不必为了使用它们而重写 upstream。

## Upstream 引入规则

直接复用不等于忽略来源和许可证：

- 优先从 `UPSTREAMS.md` 已审计 donor 中选择可运行且授权清晰的项目；
- 引入前记录仓库、精确 revision、运行时、依赖、许可证和 Asterism patch；
- 保留 upstream 许可证、版权和必要 NOTICE，明确原始文件与 Asterism Adapter；
- 无明确许可证的 donor 不直接复制进仓库，除非取得授权；可以继续作为行为资料，
  或在明确边界下调用用户自行安装的原项目；
- GPL donor 在分发或形成衍生作品前单独确认仓库整体分发方案，不用“独立进程”作为
  自动规避许可证义务的结论；
- 不提交真实凭据、可识别的账号数据、未脱敏响应、题目或答案；
- 每次真正开始接入前做一次小范围 upstream revision 和运行说明复核，但不重新开展
  大规模 clean-room 协议审计。

服务端部署使用 Git submodule 固定完整 donor，而不是要求 WebUI 用户自行 clone。
部署者应使用 `git clone --recurse-submodules`，或在已有 checkout 中执行
`git submodule update --init --recursive`。Cidaren 默认从
`upstreams/cidaren` 加载公开的 `MOPELotus/Easy_Cidaren_Backend`；命令行和环境变量
仍可覆盖 donor 路径用于审计与开发。

## 实施顺序

### 阶段 0：路线落地

- 从 `0.1.0@95875bd` 建立 `0.0.1`；
- 写明分支定位、优先级、保留面与非目标；
- 选择第一条真实 Provider vertical slice；
- 不修改产品代码。

### 阶段 1：账号与只读扫描（已完成的检查点）

同时接入 Chaoxing、WELearn、UAI、Cidaren 的固定 donor，完成账号认证和课程/任务
只读发现。题目与答案证据只在平台真实执行需要逐题处理时扫描：当前重点是
Chaoxing/Cidaren；WELearn/UAI 分别沿用 donor 的按 SCO/单元直接完成路径，时长作为
独立执行维度。该检查点不等于 Provider 完成，后续执行链继续复用同一库存和 session
边界。

### 阶段 2：只根据第一条链路修正边界

- 删除没有被真实链路使用的预设 contract 字段；
- 补足真实运行必需的配置、日志、错误和答案交换；
- 保留 UAI 私有语义，不为后续 Provider 提前泛化；
- 用同一账号重复验证只读发现、人工审核、定时 Job、取消和失败展示。

### 阶段 3：逐个接入其余 Provider（执行代码已接通）

Chaoxing、WELearn 和 Cidaren 逐个选择可工作的 donor 与最接近原始环境的 Worker。
每次先完成一个端到端真实链路，再决定下一平台；顺序可以根据账号、运行环境、
upstream 当前可用性和许可证条件调整。第二个平台真的重复第一平台的需求时，才抽取
最小共享代码。

### 阶段 4：真实使用与 0.0.1 发布

- 四个平台的主要实际功能均在授权账号上验证；
- UI 能稳定展示课程、任务、题目、Job 进度、日志和结果；
- 题库/人工审核能参与 Worker 答题，同时由 upstream 完成原生编码和提交；
- 安装说明、运行时依赖、Provider 限制和许可证材料完整；
- 只修复真实使用暴露的问题，不以终态架构完成度阻塞发布。

## 完成定义

`0.0.1` 可以存在 Provider-specific code、重复代码、不同语言和临时 Adapter。
当四个平台的主要真实流程可以从 Asterism UI 触发、观察和完成时，即满足版本目标。

以下事项不能单独阻塞发布：Worker 不统一、实现不是 Rust、已有旧抽象未被使用、缺少
灾备或 HA、未来可能需要重构。以下事项仍是硬要求：可维护的基本模块边界、外部配置、
凭据不落日志、可理解的错误、用户可见进度，以及对实际提交范围的明确授权。

## 决策记录

- **D-001：控制面/执行面分离。** Asterism 统一产品体验，upstream 保持平台执行权。
- **D-002：不做语言迁移。** Worker 采用 donor 原始运行时。
- **D-003：四平台并行到只读停止线。** 四个 Adapter 均接到账号、课程和任务扫描；
  题目扫描只用于真实需要逐题处理的 Provider。授权账号证据齐全后才进入 mutation。
- **D-004：contract 由链路生长。** 只共享已重复出现的 JSONL 进程约束、来源校验和脱敏，
  不定义终极 SDK。
- **D-005：旧代码保留但不强制。** 不删除旧架构，也不让它阻塞新的薄执行链。

## 当前 upstream Worker 状态（2026-08-24）

- 四个 Worker 均固定 donor revision 与入口 SHA-256，并实现
  `health/authenticate/courses/tasks/run`；Chaoxing/Cidaren 另提供题目读取，UAI/WELearn
  的内容读取仅保留为 Worker 私有诊断，不注册产品 Question capability；
- daemon 配置 Worker 后会自动注册 upstream-backed 的 Authentication、CourseInventory、
  TaskInventory 和已具备的 execution slot；Chaoxing/Cidaren 注册 QuestionInventory/
  QuestionParse，Chaoxing 另从已批阅结果提供 Provider-native AnswerCandidate；
- 账号 session 作为加密的 ProviderCompositeSession 进入现有 SecretStore，课程/任务扫描和
  QuestionSnapshot 复用现有控制面；
- 四个真实 donor checkout 的本地导入健康检查通过，daemon 同时注册四个平台的冒烟通过；
- 真实账号已验证 WELearn 4 门课程/797 个资源任务，UAI 2 门课程/558 个资源任务外加
  2 个课程级时长任务；第二次扫描均为零新增并全部 unchanged。UAI 已从真实任务读到
  Provider 官方精确秒数；Cidaren 保留旧库存 2 门课程/31 个任务，按当前顺序放到最后；
- Chaoxing 正式账号扫描覆盖 17 门课程、1837 个唯一远端任务：1784 个章节任务、43 个
  课程独立作业和 10 个课程 Exam，其中 576 个任务暴露 Question capability、1836 个任务
  暴露 ResourceExecution。旧 `other` 行因 source type 修正保留在本地测试库，扫描探针按
  remote id 优先 typed row 去重，不删除历史；
- Chaoxing 已批阅结果中的 `answer_evidence` 现在可经 Provider-native AnswerCandidate 边界
  导入；canonical typed-task 扫描完成 576/576，成功读取 2649 题并持久化 1243 个
  AnswerCandidate。安全探针只记录任务/快照 ID、数量和错误码，不落题干、答案或凭据；
- 2649 题按来源为章节任务 1864、课程独立作业 706、Exam 79。最终剩余 9 个只读硬阻塞：
  8 份 Exam 由教师关闭答卷查看，1 份独立作业对当前账号返回“无权限查看”；均归为
  `task_questions_unavailable`，不伪装成网络故障；
- UAI 100-task 内容 shape 诊断读取 246 个节点且零失败，仅作协议证据，不进入题库链。

### 2026-08-23 执行链增量

- Chaoxing 章节视频、音频 fallback、文档、阅读和章节 `workid` 直接调用 Samueli donor；
  课程侧栏“作业”和“考试”分别保留独立任务身份。课程 Exam 复用 CxKitty 的列表、进入、
  题目和提交顺序；课程作业普通题复用 Samueli 解码/提交，UEditor/附件题保留已审计的
  浏览器 DOM 路径；
- Chaoxing 连线、排序、共用选项、资料、口语和听力等形态在扫描阶段保留 native DOM。
  CxKitty 不能编码的 Exam 题型不会被随机答案兜底；缺少审核答案或遇到人脸、验证码、
  考试码时返回明确人工交互状态；
- Chaoxing 有题任务使用 `chaoxing.worker.answers.v1` 私有调用：WebUI 从当前不可变快照中
  收集每题明确选中的 AnswerCandidate，Core 加密保存，Worker 在 fresh question inventory
  完全一致后才接收答案。章节 Work、课程独立作业和 Exam 仍分别使用 donor 自己的编码/
  提交顺序；普通视频、文档和阅读不经过答案接口；
- 新版课程独立作业列表同时兼容单一 `enc` 和 `workEnc + stuenc` 页面，并按 donor 的
  `li[data]`/分页行为枚举。新 DOM/UEditor 作业在配置 Edge 后进入真实页面，用原生点击、
  AJAX、编辑器同步和二段提交；没有显式文件的附件题拒绝制造空附件；
- 已完成课程独立作业若先进入“作业互评”列表，Worker 只读跟随平台已有
  `/mooc2/work/eval-view` 查看入口，再复用结果页解析器；不会调用互评打分接口。真实样本
  补出填空题及 Provider-native 计算题，后者保留私有 shape，不强塞为 ShortAnswer；
- WELearn `ResourceExecution` 调用 donor `startstudy(correctness, item)`，独立
  `DurationReport` 调用 `startstudy_time`；完成写入后重新读取官方 `scoLeaves`，只有 fresh
  状态确实完成才标记 verified，复查临时失败时不重放 donor mutation；
- UAI `ResourceExecution` 调用 AutoFinish donor `process_task`，并关闭 AI、空文本和随机
  占位提交；独立秒数读取沿用 UnipusHelperPro 的 `unitTaskSituation`，实际页面驻留/视频
  则把 UnipusAIAutoPlayer 原 userscript 放入 Playwright/Edge 运行环境；
- Worker 日志和 progress 已进入 Provider Execution 事件。当前本机 bearer token 没有
  `task_execute` 权限，因此真实写入 Job 尚未验证，不能把 Worker 单测和只读扫描误报成
  远端完成验证。

### 2026-08-24 Cidaren 与 WebUI 收口

- Cidaren donor 已拆为公开纯后端子模块 `MOPELotus/Easy_Cidaren_Backend`，固定到
  `dd41afd531745c6415f2f176e5fbcd87638a70be`；不包含 Qt UI、MITM 或抓包登录工具；
- 登录只保留 WebUI 中的微信 OAuth：生成授权链接，用户在微信中验证，再把最终跳转
  URL 粘贴回 Asterism。后端校验 state/marker/origin，并保存 token 与动态加密上下文；
- Headless runner 直接保留 donor 的自学章节、班级学习任务、班级测试任务执行顺序，
  包括选词、StartAnswer、逐题答/跳过、提交、下一题和最终分数读取；Qt signal 仅替换为
  Worker progress/log 回调；
- 修正 donor 真实库存形状：自学章节来自 `task_list`，班级分页来自 `records`。两类任务
  都公开 `ResourceExecution`，完成度由 donor `progress` 映射；
- 最新 daemon 中四个 Worker health 全部通过，Cidaren health 已公开 `run`；UAI 真实账号
  再扫描保持 2 门课程、560 个任务全部 unchanged。现存 Cidaren OAuth 会话已过期，
  WELearn 现存会话被平台拒绝，因此这两个账号需重新登录后再做当季只读复验，不能把
  历史 `authenticated` 标志误报为当前在线验证；
- WebUI OpenAPI 重新生成、TypeScript 类型检查及生产构建通过。账号 OAuth、课程/任务、
  普通 Worker 执行、题目审核入口、Execution SSE 与日志页面已接到同一 API；浏览器视觉
  复验仍需有效 Web Session，不会为调试重置 Master 密码或绕过认证。
