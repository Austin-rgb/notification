#!/usr/bin/env python3
"""One-command end-to-end run.

    python tests/e2e/run_e2e.py                # build image, start stack, run tests, tear down
    python tests/e2e/run_e2e.py -k email       # extra args go to pytest
    python tests/e2e/run_e2e.py --keep         # leave the stack running afterwards
    python tests/e2e/run_e2e.py --no-up        # run against an already running stack

Requires: docker (with the compose plugin) and `pip install -r tests/e2e/requirements.txt`.
Exit code is pytest's (or 2 when the stack could not be started).
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
COMPOSE = ["docker", "compose", "-f", str(ROOT / "docker-compose.e2e.yml")]


def run(cmd, **kw):
    print("+", " ".join(cmd), flush=True)
    return subprocess.run(cmd, cwd=ROOT, **kw)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--keep", action="store_true", help="do not tear the stack down afterwards")
    parser.add_argument("--no-up", action="store_true", help="assume the stack is already running")
    parser.add_argument("--no-build", action="store_true", help="reuse the existing notification image")
    args, pytest_args = parser.parse_known_args()

    if not args.no_up:
        if shutil.which("docker") is None:
            print("docker is required (or use --no-up against a running stack)", file=sys.stderr)
            return 2
        up = COMPOSE + ["up", "-d", "--wait"] + ([] if args.no_build else ["--build"])
        if run(up).returncode != 0:
            print("\nstack failed to become healthy; recent logs:\n", file=sys.stderr)
            run(COMPOSE + ["logs", "--tail", "80"])
            run(COMPOSE + ["down", "-v"])
            return 2

    try:
        code = run([sys.executable, "-m", "pytest", "-c", str(HERE / "pytest.ini"), str(HERE)] + pytest_args).returncode
        if code != 0 and not args.no_up:
            print("\ntests failed; service logs:\n", file=sys.stderr)
            run(COMPOSE + ["logs", "--tail", "120", "notification"])
        return code
    finally:
        if not args.no_up and not args.keep:
            run(COMPOSE + ["down", "-v"])


if __name__ == "__main__":
    sys.exit(main())
