# Provider Worker

四个平台继续使用 `0.0.1` 已完成的上游薄适配器。Worker 一次读取一个 JSON 请求，输出
JSONL 进度、日志和终态事件；本地桌面控制层直接启动这些进程，不再经过旧 daemon、用户、
权限或 Service Token。

| Provider ID | donor 入口 | 操作 |
|---|---|---|
| `chaoxing` | `upstreams/chaoxing`，并辅助引用 `upstreams/chaoxing-exam` | `health`、`authenticate`、`courses`、`tasks`、`questions`、`run` |
| `welearn` | `upstreams/welearn/welearn_decompiled.py` | `health`、`authenticate`、`courses`、`tasks`、`questions`、`run`、`duration` |
| `uai` | `upstreams/uai/配置我运行我.py` | `health`、`authenticate`、`courses`、`tasks`、`inspect`、`questions`、`run`、`duration` |
| `cidaren` | `upstreams/cidaren` | `health`、`oauth_begin`、`oauth_exchange`、`authenticate`、`courses`、`tasks`、`questions`、`run` |

`welearn` 的完成率和时长由独立操作（底层仍复用 donor 的 `run.settings.action`）分开选择；
`uai` 的完成与时长也使用独立操作。`cidaren` 的 `answer_lib` 只能作为低可信历史证据。正式作业/考试答案由桌面草稿和
确认流程提供，Worker 保留原生编码与提交顺序。正式运行时桌面层可为 `cidaren` 启动仅绑定
`127.0.0.1` 的短生命周期答案桥，把新题交给全局题库/AI，并接收平台正误观察；桥接失败时
仍遵循 donor 的低可信历史答案和原生回退逻辑。

## 本地环境

从仓库根目录建立环境并安装全部现有 Worker 依赖：

```powershell
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e '.[providers,dev]'
```

运行 Worker 与本地控制层测试：

```powershell
.\.venv\Scripts\python.exe workers\run_tests.py
.\.venv\Scripts\python.exe -m unittest discover -s tests
```

验证固定 donor 能被真实导入且入口 hash 一致：

```powershell
.\.venv\Scripts\python.exe -m asterism init
.\.venv\Scripts\python.exe -m asterism health all
```

以上命令不登录平台，也不执行平台写入。真实只读扫描在全部 UI 和功能接线完成后集中执行。
