# Worker 验证记录

本文件只记录不含凭据、可重复执行的本地证据。功能和验证状态以
`docs/FEATURE_MATRIX.md` 为准。

## 当前本地证据

- 四个固定 donor 的 submodule revision 与 `SOURCE.json` 一致。
- 四个 Worker 的 `health` 均完成 donor 入口加载与 SHA-256 校验。
- Worker Fixture 共 53 项：
  - `chaoxing`：26 项，覆盖目录、富媒体/复杂题、答案形状、验证码约束、成绩构成、
    挑战标记和正式考试保存模式。
  - `welearn`：8 项，覆盖课程探测、完成率、时长、验证失败不重放和输入约束。
  - `uai`：9 项，覆盖协议、认证脱敏、课程/任务、题型、完成与独立时长。
  - `cidaren`：10 项，覆盖任务、OAuth/token、低可信历史答案、串行执行和限时策略冻结。
- 本地控制层与 GUI/AI/扫描控制层测试目前共 81 项，覆盖 Profile/会话分离、配置、SQLite、草稿双持久化、结果日志脱敏、
  Worker 错误码、超时、取消和进程回收。

## 尚未在桌面主线执行的验证

- 全部账号只读登录、课程、任务、完成状态与历史扫描将在 C4 集中执行。
- 上游已有写入能力按 `upstream-proven` 计入可用，不重复做真实平台写入。
- Asterism 扩展写入能力保持 `ported-unverified`，只做 Fixture 和代码调用链验证。

任何报告只能保存数量、类型、耗时和错误类别，不得保存课程标题、题干、答案、Cookie、
JWT、token 或 Provider-private payload。
