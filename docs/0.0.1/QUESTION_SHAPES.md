# 四 Provider 只读题型覆盖

状态：真实账号只读样本已开始回填；题库链只用于确实需要逐题处理的 Provider

## 公共映射

只有 donor 原始字段能明确表达语义时才进入已有公共题型：

| Worker 输出 | Asterism 公共模型 | 当前来源 |
|---|---|---|
| `single_choice` | SingleChoice | Chaoxing、Cidaren；UAI 仅作内容 shape 诊断 |
| `multiple_choice` | MultipleChoice | Chaoxing、Cidaren；UAI 仅作内容 shape 诊断 |
| `true_false` | TrueFalse | Chaoxing、Cidaren |
| `fill_blank` | FillBlank | Chaoxing |
| `short_answer` | ShortAnswer | Chaoxing |
| `matching` | Matching | Chaoxing |
| `ordering` | Ordering | Chaoxing |

以下输出不扩展 Shared Domain，保留完整 `native`：

- `provider_native_oral`；
- `provider_native`；
- Cidaren Unit vocabulary inventory；
- UAI 复合题、上传、讨论和专属交互节点。

## 答案证据

- Chaoxing 未完成题继续只捕获 donor 解码后的题干/选项，不调用题库查询；已完成且
  已批阅的旧题直接读取学习通结果页，把“我的答案”标成
  `chaoxing_reviewed_result` 历史证据，不经过 Asterism 重构提交协议；
- WELearn 当前 donor 的 `getscoinfo_v7` 只支撑 CMI 进度/时长读写；真实样本的
  `comment` 为 `{}`，且审计 donor 没有题目/答案证据，因此不注册 Question 能力；
- UAI donor 能解出内容/答案，但 0.0.1 的实际完成链是按单元直接执行并独立处理时长，
  不要求 Asterism 逐题答题；解析结果只保留作 Provider-native 协议诊断；
- Cidaren 首题读取不提交，因此没有标准答案证据；Unit 词表整体保留为 native inventory。

Worker 只负责给出证据，不直接把未知字段猜成公共 AnswerCandidate。授权账号扫描后，
仅当多个真实样本稳定重复且语义明确时才补映射；其余 shape 继续走 Provider-native renderer。

## 2026-08-22 真实只读扫描

- UAI：账号续期、2 门课程和 558 个任务扫描成功；100 个任务的内容 shape 诊断读取
  246 个节点且零失败，但 Question 能力不再向产品注册。
- WELearn：Cookie/密码 donor 登录经只读重试后成功，4 门课程和 797 个任务扫描成功；
  不再错误地把全部 SCO 标成可读题目。
- Cidaren：旧库存保留 2 门课程和 31 个任务；本轮 donor 明确返回 Token 已过期，未读取班级题目。
- Chaoxing：账号课程目录共 17 门（含两个同名“动物解剖学”实例）。已验证
  `有机化学实验` 29 个附件任务（11 视频、12 文档、6 测验），已批阅结果读取 39 题；
  `大学英语2级` 41 个附件任务（15 视频、22 测验、4 文档），已批阅结果读取 140 题，
  包含 4 道真实连线题；`英语听说2（2026年春学期）` 枚举 258 个唯一任务（122 视频、
  109 测验、27 文档），第一节页面另确认普通选择、判断和连线结构。扫描不提交任务。
  真实诊断发现 donor `get_job_list` 在空卡片页会隐式调用 `study_emptypage`；read-only
  Adapter 只对该 fallback 打补丁为 no-op。

Chaoxing 已完成/已截止卡片常见 `job=null`。Adapter 复用 donor 的原类型处理器恢复这些
库存项，同时保留请求级 `jobid` 与稳定 `originJobId` 的差异：某些旧卡页面要求前者为空、
后者携带卡片身份；把两者强行统一会导致只读题目请求返回 403。修正后同一真实测验从
403 恢复为 3 道单选题，未改动 donor 的未完成任务执行顺序。

## 2026-08-24 Chaoxing canonical 全量只读题目扫描

正式 Asterism 账号扫描覆盖 17 门课程、1837 个唯一 typed 任务（1784 个章节任务、43 个
课程独立作业、10 个 Exam）；576 个任务同时声明 QuestionInventory 和 AnswerResolve。
断点式只读扫描完成 576/576，共读取 2649 题：章节任务 1864、课程独立作业 706、Exam 79。
另有 9 个任务因远端权限不可读：8 份 Exam 由教师关闭答卷查看，1 份独立作业返回无权限；
这些是明确的 `task_questions_unavailable`，不是解析失败或临时网络失败。

| 题型 | 最新快照数量 |
|---|---:|
| SingleChoice | 992 |
| TrueFalse | 498 |
| FillBlank | 165 |
| MultipleChoice | 104 |
| ShortAnswer | 59 |
| Matching | 12 |
| Ordering | 7 |
| Provider-native / Unknown | 812 |

812 个 Provider-native/Unknown 中，681 个结果页节点没有独立题型标签；有标签的部分包括
`合成题1` 107、`计算题` 11、`其它` 10、`听力题` 2、`阅读理解` 1。`合成题1` 是复合/
共享上下文题族，不能压成普通单选；计算题同样保留 Provider-native。排序题继续使用已有
Ordering 映射，本轮共确认 7 道；没有扩展 Shared Domain。

最新快照中有 1244 道题带 `historical_answer_present`。Provider-native AnswerResolve
已把其中 1243 条可转换证据写入 AnswerCandidate；canonical 答案探针完成 292/292 个
含历史证据的最新 snapshot，失败为 0，数据库 AnswerCandidate 实际计数同为 1243。
差异的 1 条是 Provider-native 阅读理解，保留为不可转换的原生证据，不猜测公共答案语义。

课程独立作业还观察到“作业互评”中间页。Worker 只读跟随平台已有 `eval-view` 查看入口，
不调用互评打分；真实样本补出 11 道 FillBlank 和 14 道 Provider-native/Unknown，其中
11 道明确为计算题。计算题保留 `provider_native_calculation`/private shape，不提前扩展
Shared Domain。

扫描工具 `workers/question_scan_probe.py` 只保存任务 ID、课程标题、聚合数量、题型和
脱敏错误码，支持断点续跑；不保存题干、选项、答案、session 或凭据，也不执行远端提交。

### 当前库与历史扫描的边界

任务 source type 和全局远端身份修正后，当前活跃 Chaoxing 库为 421 个可读题任务；旧
typed task 的 576-task 全量快照仍作为 Answer History/题型研究证据保留，不篡改或搬接到
新任务身份。数据库当前累计 1700 个 QuestionSnapshot、7389 个 Question item，历史聚合
包含 Matching 36、Ordering 21；当前活跃任务中已有 45 个任务快照。后续补扫应按活跃
Task ID 记录覆盖，不把“历史已扫”误报为“当前身份已扫”，也不为统一计数直接改写快照
外键。

UAI 的 100-task 诊断样本观察到：61 个单选、13 个多选、5 个排序、22 个口语原生
节点和 145 个其他 Provider-native 节点。关键原生 shape 包括：

| task base | 节点特征 | 0.0.1 处理 |
|---|---|---|
| `single-choice` | `type=basic`，题干在 `quesText`，选项在 `options` | 映射公共 SingleChoice |
| `basic-scoop-content` | `children / contents / options / replyType / rule` | 保留 Provider-native/shared-context，不能压成普通单选 |

还观察到 `role-play`、`oral-sentence`、`oral-personal-state`、`video-dub`、`discussion`、
下拉内容、多文件上传和 `short_answer`。这些不驱动 UAI 逐题答题产品链；真正需要补的
共用选项、连线/匹配等 renderer 应优先从 Chaoxing/Cidaren 的真实逐题任务中采样。

## 真实扫描记录格式

`workers/read_only_probe.py` 默认只输出 revision、账号认证布尔值、课程/任务/题目数量、
题型计数、native inventory 数量和按类别聚合的失败。诊断时可显式启用逐课程状态，
只额外保存课程标题、教师、成功/失败和任务数；始终不输出题干、选项、答案、账号标识、
凭据或 session。
