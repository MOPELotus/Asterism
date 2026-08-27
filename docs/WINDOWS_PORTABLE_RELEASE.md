# Windows 便携发布计划

## 发布产物

CI 生成两个原生架构压缩包：

```text
asterism-windows-x64-<version>.zip
asterism-windows-arm64-<version>.zip
```

用户解压后直接双击 `Asterism.exe`。目标机器不安装 Python、Node、Rust、Visual Studio
Build Tools 或项目依赖；程序不注册服务、不修改 PATH，也不要求管理员权限。

默认使用 Nuitka `standalone`，不使用 onefile。Asterism 包含 Qt、现有 Provider Runner、
donor、浏览器自动化和 native/OCR 资源；固定的解压路径和可诊断的启动过程比单文件更重要。

## 包内结构

```text
Asterism.exe
resources/upstreams/     固定 donor（去除 .git 和开发缓存）
resources/workers/       四个平台 Runner 与元数据
resources/browsers/      CI 固定并随包携带的 Chromium
resources/licenses/      第三方许可证副本与 SOURCES.json
README.txt               首次启动、数据目录和备份说明
SHA256SUMS.json          包内文件校验清单
```

首次启动时在程序目录旁创建可写的 `accounts/`、`state/`、`drafts/`、`logs/`、`data/`
和本地配置。构建时必须使用资源 allowlist，禁止递归打包仓库、开发缓存或本地数据。固定
donor 只复制对应 revision 的 Git tracked 文件；ignored/untracked 的 Cookie、会话、日志和
本地配置不得进入 staging，tracked donor 文件被本地修改时构建直接失败。

浏览器选择顺序为随包 Chromium、系统 Microsoft Edge。无法启动浏览器时，仅相关操作显示
明确错误，其他桌面功能仍可使用；但 CI 未在对应架构启动至少一种浏览器时不得发布。

## CI 设计

- x64 在 `windows-2022` 使用 x64 Python 与 native MSVC 构建。
- ARM64 在 `windows-11-arm` 使用 ARM64 Python 与 native ARM64 MSVC 构建。
- 构建器会校验 runner 的 native machine 架构与产物标签一致；不会把 x64 runner 的产物伪标成 ARM64，
  也不尝试未经验证的交叉编译。
- 固定 Python、Nuitka 和运行依赖版本与 hash，禁止向 ARM64 包混入 x64 wheel。
- 构建前执行 `python -m playwright install chromium`，并将对应架构的 Chromium 复制到
  `resources/browsers/chromium`；如果本地构建环境没有 Chromium，包仍可生成但运行时回退系统 Edge，
  CI 发布验收必须确认随包浏览器存在并能启动。
- 按架构和锁文件 hash 缓存下载与 Nuitka 编译结果。
- 测试后构建 standalone 目录，生成 SHA-256 manifest，再制作 ZIP。
- Release Candidate 的两个架构及产物审计全部通过后才发布 GitHub Release。
- 不复用旧 Rust/Web installer 的服务、任务计划程序或机器环境安装逻辑。

实现入口为 `packaging/build_portable.py`，构建后使用
`packaging/validate_portable.py` 会先校验 SHA-256 manifest，再在含空格和中文的解压目录启动验证；
manifest 路径穿越、文件缺失、哈希不匹配或包内出现未列出的文件会在启动前失败。Workflow 仅在手动触发
或版本 tag 时运行，避免日常代码提交反复执行昂贵的双架构 Nuitka 构建。
本地验证默认允许没有随包 Chromium（运行时回退系统 Edge）；CI 发布使用 `--require-browser`，并使用独立
临时 profile 启动随包 Chromium 的 headless `--dump-dom` 烟测；进程非零退出、超时或未返回预期页面标记均会阻止产物上传。

当前 `welearn` donor 的固定 revision 未提供可确认的再分发许可证，`SOURCE.json` 仍为
`NOASSERTION`。按本计划的发布安全边界，便携构建会在实际编译前明确失败并列出阻断来源；
在确认授权或替换为许可清晰的 donor 前，不生成一个暗中缺少 `welearn` 的“完整”发布包。
构建同样会检查 `chaoxing` 的 CxKitty 辅助 donor 和 `uai` 的浏览器脚本 manifest，二者的
许可证必须显式记录在 metadata 中。所有主 donor 和辅助 donor 还必须通过入口文件 SHA-256
校验；在 Git checkout 中同时校验 submodule `HEAD` 与固定 revision，源码归档没有 `.git`
元数据时仍必须通过文件哈希校验。任一来源缺失、内容漂移或 revision 不一致都在 Nuitka
编译前终止构建。

## 必须显式打包的资源

- PyQt6、Fluent 主题和图标资源。
- TLS CA、OCR 引擎/模型、OpenCV/NumPy 等 native 库。
- Playwright driver、许可允许携带的浏览器和启动配置。
- 四个平台现有 Runner、固定 donor、运行时数据和必要 Fixture。
- Asterism 与全部第三方版权、许可证和 notice。

每个 donor 都必须记录准确仓库、revision 和许可证。未解决再分发许可的 donor 不进入 ZIP，
对应能力标记为 `blocked`，不得通过猜测协议重新实现来掩盖。

## 干净环境验收

每个架构的 CI 将 ZIP 分别解压到 ASCII 路径和包含空格、中文的路径，然后验证：

1. `Asterism.exe` 能启动，第二实例不会形成冲突的数据写入者。
2. 首次启动能建立目录和空题库 SQLite。
3. Profile 新建、修改、删除和状态原子写入无需提权。
4. 四个 Runner 能加载固定来源并返回不含凭据的 health 结果。
5. Fixture 运行能输出进度/日志、可取消，结束后没有遗留子进程。
6. SQLite、草稿、备份和重启恢复正常。
7. 随包 Chromium 或系统 Edge 能由实际浏览器适配层启动。
8. 日志、崩溃输出、ZIP 和 SHA-256 manifest 不含凭据或构建机路径。
9. 断网时应用仍能启动，Provider 显示有限且明确的网络错误。

发布 CI 不执行真实平台写入。C4 的真实账号只读扫描单独记录状态，账号、响应和答案不得
进入 Fixture、日志附件或发布产物。
