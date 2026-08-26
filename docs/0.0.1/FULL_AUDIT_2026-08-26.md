# 0.0.1 二次全仓审计记录（2026-08-26）

本文记录本轮实现后的第二次逐流程/逐功能/逐模块复核，不把测试通过误写成真实平台验证完成。

## 已有证据

- `cargo test --workspace`：全 workspace 通过；API 85、Engine 163、Storage 173，四 Provider 与其余 crate 测试均通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过。
- Web `npm run typecheck`、`npm run build`：通过。
- Yunzai 插件 `npm test`：11 项通过；`npm run check` 的 Node 语法边界保持可执行。
- Python Worker：Chaoxing 25、WELearn 5、UAI 9、Cidaren 10 项单元测试通过。
- 新增 API 测试：QQ master assertion 的 scope 约束、单向提权和审计；master 使用 `X-Asterism-Target-Owner` 读取其他用户 Provider Account。

## 本轮已收口

1. QQ assertion 的 `master_assertion` 仅能由 Yunzai 插件根据 `e.isMaster` 产生；服务端要求 `qq_identity_assert + task_command_proxy`，普通用户无法自行升级。
2. Web API 的 target owner 在 account、course、task、execution、题库、浏览器桥、AI、扫描、课程自动化和点数读取等共享授权入口统一解析；普通用户只能选择自己，管理员需要相应全局权限。
3. WebUI 顶栏提供管理员代操作用户选择器，并通过同源请求头传递目标 owner；清空后恢复自己的资源。
4. Windows 安装器具备依赖检测/winget 安装、Python venv、浏览器探测、构建、密钥 ACL、Yunzai 复制、任务注册、健康检查和初始账号向导；反代明确排除。

## 仍然必须现场或专项验收的项

- Windows Server 干净机、已有依赖、路径含中文/空格、断网/端口冲突、重复升级、系统重启后的安装器实际回归。
- 四个平台真实账号登录、课程/任务只读、Chaoxing 三类验证码、全量静默扫描和真实题型/附件混编。
- Yunzai 真实 Miao/TRSS 实例中的 `e.isMaster` 事件值、锅巴配置、群 @ 投递和 Service Token scope。
- 代用户执行的每一个写路由必须在真实 WebUI 操作中确认 owner、actor、通知和扣点；当前已有共享后端边界和基础测试，但仍需逐页现场验收。
- OpenAPI 自定义 target-owner header 的客户端契约说明和最终部署手册联动检查。

## 结论

代码层面主干、权限基础和安装向导已经继续推进，但以上现场/专项项未完成前，不能宣称“全部功能已验证、可无条件生产部署”。下一检查点应优先完成 Windows 实机安装与四 Provider 只读现场回归，再重新运行本文档矩阵。
