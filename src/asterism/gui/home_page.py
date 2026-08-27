from __future__ import annotations

from typing import Any

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QGridLayout, QVBoxLayout, QWidget

from ..constants import PROVIDER_IDS
from .common import display_provider, make_title
from .controller import DesktopController
from .fluent import (
    BodyLabel,
    CaptionLabel,
    CardWidget,
    PrimaryPushButton,
    StrongBodyLabel,
    wrapped_caption,
)


class HomePage(QWidget):
    def __init__(self, controller: DesktopController):
        super().__init__()
        self.controller = controller
        self._active_activities: dict[str, str] = {}
        self._last_activity = "暂无运行中的任务"
        layout = QVBoxLayout(self)
        layout.setContentsMargins(28, 24, 28, 28)
        layout.setSpacing(16)

        hero = CardWidget()
        hero_layout = QVBoxLayout(hero)
        hero_layout.setContentsMargins(24, 20, 24, 20)
        hero_layout.setSpacing(6)
        hero_layout.addWidget(make_title("Asterism"))
        hero_layout.addWidget(BodyLabel("本地桌面控制台"))
        hero_layout.addWidget(
            wrapped_caption("选择平台账号，完成认证后读取课程和任务。正式提交始终需要明确确认。")
        )
        layout.addWidget(hero)

        metrics = QGridLayout()
        metrics.setHorizontalSpacing(12)
        metrics.setVerticalSpacing(12)
        self.metric_labels: dict[str, Any] = {}
        for column, (key, title, note) in enumerate(
            (
                ("profiles", "本地账号", "四个平台的账号总数"),
                ("courses", "课程缓存", "当前本地已读取的课程"),
                ("tasks", "任务缓存", "当前本地已读取的任务"),
                ("bank", "题库题目", "全局题库中的题目数量"),
            )
        ):
            card = CardWidget()
            card_layout = QVBoxLayout(card)
            card_layout.setContentsMargins(16, 14, 16, 14)
            card_layout.setSpacing(3)
            card_layout.addWidget(CaptionLabel(title))
            value = make_title("0")
            value.setObjectName(f"metric_{key}")
            card_layout.addWidget(value)
            card_layout.addWidget(CaptionLabel(note))
            metrics.addWidget(card, column // 2, column % 2)
            self.metric_labels[key] = value
        layout.addLayout(metrics)

        overview = CardWidget()
        overview_layout = QVBoxLayout(overview)
        overview_layout.setContentsMargins(20, 16, 20, 16)
        overview_layout.setSpacing(8)
        overview_layout.addWidget(StrongBodyLabel("运行概况"))
        self.summary = BodyLabel()
        self.summary.setWordWrap(True)
        overview_layout.addWidget(self.summary)
        self.refresh = PrimaryPushButton("刷新本地状态")
        self.refresh.clicked.connect(self.update_summary)
        overview_layout.addWidget(self.refresh, 0, Qt.AlignmentFlag.AlignLeft)
        layout.addWidget(overview)

        activity = CardWidget()
        activity_layout = QVBoxLayout(activity)
        activity_layout.setContentsMargins(20, 16, 20, 16)
        activity_layout.setSpacing(5)
        activity_layout.addWidget(StrongBodyLabel("当前运行状态"))
        self.activity = BodyLabel("暂无运行中的任务")
        self.activity.setWordWrap(True)
        activity_layout.addWidget(self.activity)
        layout.addWidget(activity)

        tips = CardWidget()
        tips_layout = QVBoxLayout(tips)
        tips_layout.setContentsMargins(20, 16, 20, 16)
        tips_layout.setSpacing(5)
        tips_layout.addWidget(StrongBodyLabel("开始使用"))
        tips_layout.addWidget(
            CaptionLabel("1. 打开左侧平台页面；2. 新建平台账号；3. 点击认证 / 登录；4. 读取课程。")
        )
        tips_layout.addWidget(
            CaptionLabel(
                "课程、任务和题目读取不会提交平台数据；作业和考试需在草稿页人工确认后才能提交。"
            )
        )
        layout.addWidget(tips)
        layout.addStretch(1)
        self.update_summary()

    def update_summary(self) -> None:
        counts = self.controller.dashboard_counts()
        provider_counts = {
            provider: len(self.controller.profiles.list(provider)) for provider in PROVIDER_IDS
        }
        for name in ("profiles", "courses", "tasks", "bank"):
            self.metric_labels[name].setText(str(counts.get(name, 0)))
        self.summary.setText(
            "账号："
            + "，".join(
                f"{display_provider(provider)} {provider_counts[provider]}"
                for provider in PROVIDER_IDS
            )
            + f"\n数据目录：{self.controller.paths.root}\n"
            "从平台页完成认证后，课程和任务会显示在对应页面。"
        )

    def set_activity(
        self,
        text: str,
        *,
        current: object | None = None,
        total: object | None = None,
        finished: bool = False,
        activity_id: str = "global",
    ) -> None:
        if finished:
            self._active_activities.pop(activity_id, None)
            self._last_activity = f"最近完成：{text}"
        else:
            progress = ""
            if current is not None:
                progress = (
                    f" · {current}/{total}" if total not in (None, "") else f" · {current}"
                )
            self._active_activities[activity_id] = f"正在执行：{text}{progress}"
        self.activity.setText(
            "\n\n".join(self._active_activities.values())
            if self._active_activities
            else self._last_activity
        )
