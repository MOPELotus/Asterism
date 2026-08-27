from __future__ import annotations

import threading
from collections.abc import Callable
from concurrent.futures import Future, ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from typing import Any

from .profiles import Profile
from .runner import RunnerError
from .service import ProviderOperationResult, ProviderService


@dataclass(frozen=True)
class BatchItemResult:
    index: int
    task_remote_id: str
    result: ProviderOperationResult | None = None
    error_code: str | None = None
    error_message: str | None = None


class ManualBatchExecutor:
    def __init__(self, service: ProviderService) -> None:
        self.service = service

    def run(
        self,
        profile: Profile,
        tasks: list[dict[str, Any]],
        *,
        concurrency: int = 1,
        settings: dict[str, Any] | None = None,
        answer_provider: Callable[[dict[str, Any]], list[dict[str, Any]] | None] | None = None,
        cancel: threading.Event | None = None,
        run_task: Callable[..., ProviderOperationResult] | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> list[BatchItemResult]:
        if concurrency < 1:
            raise ValueError("concurrency must be at least one")
        if profile.provider in {"uai", "cidaren"}:
            concurrency = 1
        cancellation = cancel or threading.Event()

        def execute(index: int, task: dict[str, Any]) -> BatchItemResult:
            remote_id = str(task.get("remote_id") or index)
            if cancellation.is_set():
                return BatchItemResult(
                    index, remote_id, error_code="cancelled", error_message="batch cancelled"
                )
            try:
                answers = answer_provider(task) if answer_provider else None
                execute_task = run_task or self.service.run_task
                if on_event is None:
                    result = execute_task(
                        profile,
                        task,
                        answers=answers,
                        settings=settings,
                        cancel=cancellation,
                    )
                else:
                    def forward(event: dict[str, Any]) -> None:
                        enriched = dict(event)
                        enriched.setdefault("batch_index", index)
                        enriched.setdefault("task_remote_id", remote_id)
                        on_event(enriched)

                    result = execute_task(
                        profile,
                        task,
                        answers=answers,
                        settings=settings,
                        cancel=cancellation,
                        on_event=forward,
                    )
                return BatchItemResult(index, remote_id, result=result)
            except RunnerError as error:
                return BatchItemResult(
                    index, remote_id, error_code=error.code, error_message=str(error)
                )
            except (OSError, RuntimeError, ValueError) as error:
                return BatchItemResult(
                    index,
                    remote_id,
                    error_code="local_error",
                    error_message=str(error),
                )
            except Exception as error:
                # A provider-specific answer adapter must not abort the whole
                # batch when it raises an unexpected local exception. Keep the
                # item isolated and avoid echoing arbitrary exception text,
                # which may contain credentials or remote payloads.
                return BatchItemResult(
                    index,
                    remote_id,
                    error_code="local_error",
                    error_message=f"{type(error).__name__}: batch item failed",
                )

        results: list[BatchItemResult] = []
        with ThreadPoolExecutor(
            max_workers=concurrency, thread_name_prefix="asterism-batch"
        ) as pool:
            futures: list[Future[BatchItemResult]] = [
                pool.submit(execute, index, task) for index, task in enumerate(tasks)
            ]
            for future in as_completed(futures):
                results.append(future.result())
        return sorted(results, key=lambda item: item.index)
