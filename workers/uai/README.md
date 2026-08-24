# Asterism UAI Worker

这是 `0.0.1` 路线的 UAI 专用薄 Adapter。它运行固定的
`create-try-now/AutoFinish_UxiaoyuanAI` Python 入口，并通过 stdin/stdout JSON Lines
与 Asterism 通信。登录、请求、解密、任务遍历、答案编码和提交仍由 donor 拥有。

当前接入 revision 和入口哈希记录在 `SOURCE.json`。Worker 启动时会核对 SHA-256；
不匹配的 donor 文件不会执行。

## 环境

建议建立独立 Python 环境：

```powershell
python -m venv target/uai-worker-venv
target/uai-worker-venv/Scripts/python.exe -m pip install -r workers/uai/requirements.txt
```

准备 `SOURCE.json` 中指定 revision 的 donor checkout，并把其入口文件路径传给 Worker。
donor 本身不由这个目录重新实现。

## 独立健康检查

```powershell
'{"request_id":"health-1","operation":"health","payload":{}}' |
  target/uai-worker-venv/Scripts/python.exe workers/uai/worker.py `
    --upstream '<donor-checkout>/配置我运行我.py'
```

成功时只输出 JSON Lines 事件，最终事件的 `type` 为 `result`，并包含 donor revision、
入口哈希、许可证、Python 版本和当前操作列表。

## daemon 配置

最少只需设置 donor 入口；其他路径有仓库开发默认值：

```powershell
$env:ASTERISM_UAI_WORKER_UPSTREAM = '<donor-checkout>/配置我运行我.py'
$env:ASTERISM_UAI_WORKER_PYTHON = 'target/uai-worker-venv/Scripts/python.exe'
cargo run -p asterismd
```

等价 CLI 参数包括：

- `--uai-worker-upstream`；
- `--uai-worker-python`；
- `--uai-worker-adapter`；
- `--uai-worker-source-metadata`；
- `--uai-worker-timeout-seconds`。

配置后，具有 Provider read 权限的用户可以请求：

```text
GET /api/v1/providers/uai/worker/health
```

未配置或 Worker 不可用时返回 503，但不降低 `/api/v1/system/health` 的状态。

## 当前边界

Python Adapter 已实现：

- `health`：加载依赖、核对 donor 哈希和方法 surface；
- `authenticate`：注入账号密码并调用 donor `login()`；
- `courses`：复用 donor session 读取课程和教程；
- `tasks`：复用 donor 结构、完成状态和递归遍历；
- `inspect`：读取一个新鲜 Task 的 content 和 ProviderNative 标准答案。

Rust 侧的通用进程客户端和 upstream-backed read-only Provider 已接入现有
Provider Account、SecretStore、Course/Task scan 与 QuestionSnapshot 链；JWT/Cookie
只作为加密的 `ProviderCompositeSession` 保存，不暴露给 WebUI。API 公开受保护的
worker health，账号认证和扫描继续使用现有 Provider Account API。

离线 fixture 已跑通 `authenticate -> courses -> tasks -> questions`，但仓库环境没有授权
真实账号凭据，因此真实 UAI shape 仍需用 `workers/read_only_probe.py` 验证。

普通日志不得包含密码、JWT、Cookie、annotator token、题目/答案原文或完整 native
payload。真实验证数据不提交到仓库。
