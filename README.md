# Asterism

Asterism 正在重构为一款 Windows 优先的本地桌面工具，用于统一管理和执行 `chaoxing`、
`welearn`、`uai` 和 `cidaren` 平台上的多个账号。

新主线不重新实现已经存在的平台功能，而是直接复用 `0.0.1` 中已经迁移的代码和经审计
上游项目的成熟逻辑，移除多用户运营层后接入新的本地桌面界面。此前的多用户 WebUI/服务
架构完整保存在 `0.0.1` 分支。

> 当前 `master` 仅包含路线和范围文档，桌面功能迁移尚未开始，暂不可直接运行。

## 产品方向

- 单一可信本地操作者，无 Asterism 登录、角色或权限系统。
- 四个平台均可配置任意多个本地账号 Profile。
- 使用 PyQt6 与 PyQt6-Fluent-Widgets 构建模块化 Fluent 桌面界面。
- 原样复用现有 Provider、Worker、题型解析、答案策略和平台私有执行顺序。
- 保留课程、任务、题库、AI、复杂题、历史扫描、作业/考试草稿与提交前确认。
- 移除用户管理、代操作、QQ 绑定、点数计费、Service Token、公开 API 和自动巡检。
- 提供无需安装 Python、Node、Rust 或编译工具的 Windows x64/ARM64 便携 ZIP。

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

对外文档以简体中文为主；代码标识、配置键、Runner 协议和 Git 提交信息使用英文。产品
稳定后再补充面向国际开发者的 `README.en.md`。
