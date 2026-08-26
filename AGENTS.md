# Asterism Agent 工作约束

本文件适用于本地桌面版主线上的所有 Agent。

## 产品边界

Asterism 是 Windows 优先、单一可信操作者使用的本地桌面工具，支持 `chaoxing`、
`welearn`、`uai` 和 `cidaren` 各自任意多个平台账号。不得重新引入 Asterism 用户、角色、权限、代操作、
QQ 绑定、点数计费、公开 API、Service Token、自动巡检或多租户所有权。

平台账号是本地 Profile，不是应用用户。账号明文允许存入已忽略的本地配置，但绝不能
进入日志、提交、Fixture、发布元数据或崩溃报告。

## 复用优先，不重新迁移

`0.0.1` 是 Provider、Worker、题库、AI、任务、草稿、Fixture、研究资料和已确认语义的
迁移来源。经审计 donor 是平台执行能力的首选来源。

- 能修改导入、调用或配置就直接复用现有模块。
- 只有现有边界无法供桌面层调用时才增加薄适配层。
- 不将可工作的 Python、JavaScript、Rust 或浏览器流程改写成另一种语言。
- 不为了统一接口重构 donor 状态机、平台私有顺序或题型语义。
- 不重复真实写入测试上游已经稳定提供的能力。
- 不迁回旧 WebUI、运营数据库、公开 API、Scheduler 或权限系统作为捷径。

旧代码需要 actor/owner/权限时，本地模式应提供唯一固定操作者，或绕过权限外壳调用下层
业务服务；不得构造一套只为兼容旧接口存在的伪多用户系统。

## 功能完整性与验证语义

删除服务运营层不得删除真实功能。必须保留课程与任务发现、所有有依据的完成操作、题库、
答案证据、AI、复杂富媒体题、历史扫描、挑战与验证码、正式作业/考试草稿和提交确认、
进度、日志、取消及可选终态通知。

状态必须依据 `docs/IMPLEMENTATION_PLAN.md` 区分 `upstream-proven`、
`ported-unverified`、`desktop-wired`、`fixture-verified`、`live-read` 和 `live-write`。
代码存在不等于真实验证；没有真实写入条件也不等于功能未实现。

Provider 私有行为和适度重复可以保留。只有多个平台出现真实重复需求时才抽共享代码。

## 桌面结构

- 使用 PyQt6 与 PyQt6-Fluent-Widgets 构建模块化页面和组件，禁止巨型单文件 UI。
- 优先保留 `0.0.1` 中已有的 Provider 进程、Worker 和调用边界。
- UI 只负责选择、生命周期、进度、日志、设置和人工确认，不实现 Provider 协议。
- 若确需新增进程边界，使用最薄的 JSON/JSONL stdin/stdout 协议。
- Profile、状态、日志、草稿和题库分别放在 `accounts/`、`state/`、`logs/`、
  `drafts/` 和 `data/`。
- SQLite 只用于全局题库、答案证据、AI 缓存和正式任务草稿，不得扩展成用户、权限、
  Scheduler 或分布式 Job 数据库。
- 不增加自动调度。保留手动单次/批量执行；通知仅为可选的成功/失败终态通知。

## 交付与安全

Windows x64 和 ARM64 使用 Nuitka standalone 便携 ZIP 发布。解压后必须能运行，不要求
Python、Node、Rust、编译、管理员权限、服务注册或 PATH 修改。

发布包必须保留 donor 的准确版本、许可证和版权信息。未解决再分发许可的 donor 不得打包，
对应功能应明确标记为受阻，不得用猜测协议静默替换。

提交信息使用英文 Conventional Commits，例如：
`feat(chaoxing): wire formal work drafts into desktop`。
