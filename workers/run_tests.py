"""Run the four thin-worker fixture suites without requiring pytest.

This intentionally imports test files by path so the worker directories do not
need to become Python packages and donor dependencies remain optional.
"""
from __future__ import annotations

import importlib.util
import pathlib
import sys
import unittest


ROOT = pathlib.Path(__file__).resolve().parent


def load_suite(path: pathlib.Path) -> unittest.TestSuite:
    name = "asterism_worker_test_" + path.parent.parent.name
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return unittest.defaultTestLoader.loadTestsFromModule(module)


def main() -> int:
    suite = unittest.TestSuite()
    paths = sorted(ROOT.glob("*/tests/test_worker.py"))
    if not paths:
        print("no worker test files found", file=sys.stderr)
        return 2
    for path in paths:
        suite.addTests(load_suite(path))
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
