# Asterism

Asterism 正在重构为一款 Windows 优先的本地桌面工具，用于统一管理和执行 `chaoxing`、
`welearn`、`uai` 和 `cidaren` 平台上的多个账号。

新主线不重新实现已经存在的平台功能，而是直接复用 `0.0.1` 中已经迁移的代码和经审计
上游项目的成熟逻辑，移除多用户运营层后接入新的本地桌面界面。此前的多用户 WebUI/服务
架构完整保存在 `0.0.1` 分支。

> 当前 `master` 已完成本地运行骨架和首版控制面接线；桌面功能仍在迁移中，暂不作为正式发布包。

Windows 便携构建骨架已经接入，但固定的 `welearn` donor 尚无可确认的再分发许可证，因此
默认完整发布会主动阻止不合规 ZIP；也可以使用外部 donor 模式，不把该平台源码放进 ZIP，
首次使用前由操作者从固定 revision 的仓库自行准备或下载 donor。

## 产品方向

- 单一可信本地操作者，无 Asterism 登录、角色或权限系统。
- 四个平台均可配置任意多个本地账号 Profile。
- 使用 PyQt6 与 PyQt6-Fluent-Widgets 构建模块化 Fluent 桌面界面。
- 主窗口采用免费版 `FluentWindow`/`MSFluentWindow` 与 `NavigationInterface`；页面控件统一使用
  免费 Fluent 组件，Pro 组件不作为依赖。
- 支持跟随系统、浅色、深色主题和 Windows 高 DPI 显示缩放。
- 原样复用现有 Provider、Worker、题型解析、答案策略和平台私有执行顺序。
- 题库按题干、材料、选项内容和媒体语义匹配，忽略随机选项顺序及远端 ID；答案和 AI 响应只写入本机缓存。
- 保留课程、任务、题库、AI、复杂题、历史扫描、作业/考试草稿与提交前确认；题库在执行过程中自动积累，
  不要求用户单独维护题库页面；正式任务先读取并预填
  可确定答案，未解析题目必须由操作者补漏后才能提交。
- 移除用户管理、代操作、QQ 绑定、点数计费、Service Token、公开 API 和自动巡检。
- 提供无需安装 Python、Node、Rust 或编译工具的 Windows x64/ARM64 便携 ZIP。
- 首次启动会显示本地数据目录和安全边界向导；确认后即可在各平台页创建 Profile。

## 实施原则

- 能改引用和薄适配就不复制或重写实现。
- 上游已经稳定工作的能力可标记为 `upstream-proven`，不重复进行真实写入试验。
- Asterism 此前扩展的写入能力在代码接通后标记为 `ported-unverified`，等待以后有条件验证。
- 全部功能接线和 UI 完成后，再统一执行一次四平台真实只读扫描。
- 功能状态与验证状态分开记录；“未做真实写入”不等于“未实现”。

## 文档

- [实施计划](docs/IMPLEMENTATION_PLAN.md)
- [功能接线矩阵](docs/FEATURE_MATRIX.md)
- [Windows 便携发布计划](docs/WINDOWS_PORTABLE_RELEASE.md)

对外文档以简体中文为主；代码标识、配置键、Runner 协议和 Git 提交信息使用英文。当前
仍处于迁移阶段，只有代码、fixture 和上游证据标记为已验证；真实平台写入不在本阶段验收范围。
