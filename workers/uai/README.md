# `uai` Worker

该 Worker 直接加载固定 donor `upstreams/uai/配置我运行我.py`，保留其登录、课程、任务和
执行顺序。桌面层只负责提供 Profile、选择任务并显示事件。

完成操作为 `run`；时长读取为 `duration`，课程驻留时长写入仍由带
`route_kind=course_duration` 的 `run` 单独执行。两条路径不得隐式合并。

Worker 不启用 donor 的通用 AI 配置；确实需要生成讨论等内容时，由本地共享答案策略提供
经过确认的文本。
