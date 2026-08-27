from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from .config import LocalConfigStore
from .constants import PROVIDER_IDS
from .database import QuestionBank
from .paths import DataPaths, application_root
from .profiles import ProfileStateStore, ProfileStore
from .providers import ProviderRegistry
from .runner import RunnerError, RunnerManager
from .upstreams import resolve as resolve_upstream


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="asterism", description="Asterism local desktop diagnostics"
    )
    parser.add_argument("--data-root", type=Path, help="override writable local data root")
    parser.add_argument("--source-root", type=Path, help="override application resource root")
    command = parser.add_subparsers(dest="command", required=True)
    command.add_parser("init", help="create local directories and question-bank database")
    health = command.add_parser("health", help="load pinned Provider workers without an account")
    health.add_argument("provider", choices=(*PROVIDER_IDS, "all"), default="all", nargs="?")
    profile = command.add_parser("profile", help="manage plaintext local Provider profiles")
    profile_command = profile.add_subparsers(dest="profile_command", required=True)
    profile_new = profile_command.add_parser(
        "new", help="create an editable credential-free profile"
    )
    profile_new.add_argument("provider", choices=PROVIDER_IDS)
    profile_new.add_argument("label")
    profile_list = profile_command.add_parser("list", help="list profiles without credentials")
    profile_list.add_argument("--provider", choices=PROVIDER_IDS)
    upstream = command.add_parser("upstream", help="inspect or install a pinned upstream")
    upstream_command = upstream.add_subparsers(dest="upstream_command", required=True)
    upstream_install = upstream_command.add_parser(
        "install", help="install a pinned provider donor"
    )
    upstream_install.add_argument("provider", choices=PROVIDER_IDS)
    upstream_install.add_argument(
        "--network", action="store_true", help="allow GitHub clone/archive download"
    )
    return parser


def _context(args: argparse.Namespace):
    paths = DataPaths.resolve(args.data_root)
    source_root = (args.source_root or application_root()).resolve()
    registry = ProviderRegistry(source_root, data_root=paths.root)
    profiles = ProfileStore(paths)
    states = ProfileStateStore(paths)
    runner = RunnerManager(registry, paths.logs, states)
    return paths, registry, profiles, runner


def _initialize(paths: DataPaths) -> None:
    paths.initialize()
    LocalConfigStore(paths.config).ensure()
    QuestionBank(paths.database).initialize()


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    paths, registry, profiles, runner = _context(args)
    try:
        if args.command == "init":
            _initialize(paths)
            print(json.dumps({"status": "ok", "data_root": str(paths.root)}, ensure_ascii=False))
            return 0
        if args.command == "profile":
            _initialize(paths)
            if args.profile_command == "new":
                profile = profiles.create(args.provider, args.label)
                print(
                    json.dumps(
                        {
                            "id": profile.id,
                            "provider": profile.provider,
                            "label": profile.label,
                            "path": str(profiles.path_for(profile.provider, profile.id)),
                        },
                        ensure_ascii=False,
                    )
                )
                return 0
            values = [
                {
                    "id": profile.id,
                    "provider": profile.provider,
                    "label": profile.label,
                    "enabled": profile.enabled,
                }
                for profile in profiles.list(args.provider)
            ]
            print(json.dumps(values, ensure_ascii=False))
            return 0
        if args.command == "upstream":
            _initialize(paths)
            if args.upstream_command == "install":
                resolved = resolve_upstream(
                    registry.source_root,
                    paths.root,
                    args.provider,
                    allow_network=args.network,
                )
                print(
                    json.dumps(
                        {"status": "ok", "provider": args.provider, "path": str(resolved)},
                        ensure_ascii=False,
                    )
                )
                return 0
        if args.command == "health":
            _initialize(paths)
            specs = registry.all() if args.provider == "all" else (registry.get(args.provider),)
            failed = False
            results = []
            for spec in specs:
                try:
                    result = runner.invoke(spec, "health", timeout=30)
                    results.append({"provider": spec.provider, "status": "ok", "data": result.data})
                except RunnerError as error:
                    failed = True
                    results.append(
                        {
                            "provider": spec.provider,
                            "status": "error",
                            "code": error.code,
                            "message": str(error),
                        }
                    )
            print(json.dumps(results, ensure_ascii=False))
            return 1 if failed else 0
    except (OSError, ValueError, RuntimeError) as error:
        print(
            json.dumps({"status": "error", "message": str(error)}, ensure_ascii=False),
            file=sys.stderr,
        )
        return 2
    raise AssertionError("unreachable command")
