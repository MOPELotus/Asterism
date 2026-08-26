from __future__ import annotations

import argparse
import json
import sys
import time

parser = argparse.ArgumentParser()
parser.add_argument("--upstream")
parser.add_argument("--source-metadata")
parser.parse_args()

request = json.loads(sys.stdin.readline())
base = {
    "protocol": "asterism.fake.worker.v1",
    "request_id": request["request_id"],
    "operation": request["operation"],
}
if request["operation"] == "sleep":
    time.sleep(10)
elif request["operation"] == "fail":
    print(
        json.dumps({**base, "type": "error", "code": "expected", "message": "expected failure"}),
        flush=True,
    )
    raise SystemExit(2)
else:
    print(json.dumps({**base, "type": "progress", "current": 1, "total": 1}), flush=True)
    print(
        json.dumps(
            {
                **base,
                "type": "result",
                "data": {
                    "session": {"token": "fixture-secret-must-not-enter-log"},
                    "items": [{"prompt": "fixture question body"}],
                },
            }
        ),
        flush=True,
    )
