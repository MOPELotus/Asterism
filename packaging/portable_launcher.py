from __future__ import annotations

import sys
from pathlib import Path


def main() -> int:
    from asterism.gui.app import main as gui_main

    application_root = Path(sys.executable).resolve().parent
    return gui_main(
        [
            "--data-root",
            str(application_root),
            "--source-root",
            str(application_root / "resources"),
        ]
    )


if __name__ == "__main__":
    raise SystemExit(main())
