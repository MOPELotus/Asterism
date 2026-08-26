# 0.0.1 upstream workers

四个平台都使用外部 donor checkout；仓库只保存薄 Adapter、固定 revision/hash 和
传输客户端。当前 `0.0.1` Worker 已接入账号认证、课程/任务扫描以及按平台划分的
execution 边界；Chaoxing/Cidaren 支持受策略控制的答题或提交，WELearn/UAI 将完成率
与时长作为独立动作。代码、fixture 和本地验收不等于每个平台的实时账号 mutation 已验证。

| Provider | donor 输入 | 登录 | 只读扫描 | 题目扫描边界 |
|---|---|---|---|---|
| Chaoxing | checkout 根目录 | 密码、Cookie 或浏览器辅助认证 | 课程、章节、任务卡、题目/答案证据 | 章节资源、作业/考试和挑战点沿用 donor；正式测评要求审核后的答案与独立确认 |
| WELearn | `welearn_decompiled.py` | 2026 SSO 密码或 Cookie | 课程、Unit、SCO | `complete` 与 `duration` 为独立 Worker 动作，直接复用 donor 的 CMI/进度接口 |
| UAI | `配置我运行我.py` | 密码/JWT | 教程、结构、完成状态、时长 | 课程资源完成与页面驻留时长分别调用 donor；讨论文本受 AI 配置与确认策略约束 |
| Cidaren | checkout 根目录 | 微信 OAuth/token | 当前课程、Unit、班级任务 | timed/instant 答案桥接与限时回退；`answer_lib` 仅作为低可信历史证据，不能单独视为正确 |

## 环境

建议每个 Worker 使用自己的虚拟环境，并安装对应 donor 的原始 requirements。WELearn
薄脚本只需要 `requests`；Cidaren 的 headless Adapter 会为 donor 未实际使用的 Qt 对话框
提供空壳，因此只读链不要求安装 PyQt6。不要把 donor checkout 放入 git。

daemon 共享以下运行参数：

- `--uai-worker-python`：当前四个 Python Worker 共用的解释器；
- `--uai-worker-timeout-seconds`：单次子进程硬超时；
- `--chaoxing-worker-upstream`：Chaoxing checkout 根目录；
- `--welearn-worker-upstream`：WELearn donor Python 文件；
- `--uai-worker-upstream`：UAI donor Python 文件；
- `--cidaren-worker-upstream`：Cidaren checkout 根目录。

配置后，受 Provider-read 权限保护的健康入口为：

```text
GET /api/v1/providers/{chaoxing|welearn|uai|cidaren}/worker/health
```

## 授权账号只读探针

凭据只从进程环境读取，报告只输出数量、题型和错误类别：

```powershell
$env:ASTERISM_WORKER_USERNAME = '<local-only>'
$env:ASTERISM_WORKER_PASSWORD = '<local-only>'
target/uai-worker-venv/Scripts/python.exe workers/read_only_probe.py uai `
  --python target/uai-worker-venv/Scripts/python.exe `
  --adapter workers/uai/worker.py `
  --upstream '<donor>/配置我运行我.py' `
  --source-metadata workers/uai/SOURCE.json
```

Chaoxing/WELearn 也可用 `ASTERISM_WORKER_COOKIE`。Cidaren 使用
`ASTERISM_WORKER_TOKEN`。Cidaren 班级题目的首题扫描还需显式传入
`--allow-read-that-starts-attempt`；未授权时探针只扫描 Unit 原生词表。

探针不会提交答案、进入下一题或完成任务。真实报告不得提交课程标题、题干、答案、
Cookie/JWT/token 或 native payload。
