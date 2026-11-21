#!/usr/bin/env python3
"""Create (or reset) a throwaway project OUTSIDE this repo for trying agents that can write and run.

    python examples/sandbox.py            # create ../loom-sandbox next to this repo
    python examples/sandbox.py --reset    # delete and recreate it
    python examples/sandbox.py D:\\somewhere\\else

It is a tiny Node project (no dependencies) with a git history and one failing test, so tasks like
"fix the failing test" have something real to do. Then point the agent at it:

    python examples/agent.py --model qwen3:8b --root ../loom-sandbox --allow-write --allow-run \\
        "Run the tests, find out why one fails, fix it, and run the tests again"

Safety: a folder is only ever deleted if it carries the marker file this script writes, and never
if it is this repo, inside it, or a parent of it.
"""

import argparse
import os
import shutil
import stat
import subprocess
import sys

MARKER = ".loom-sandbox"
REPO = os.path.realpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

FILES = {
    ".gitignore": f"{MARKER}\nnode_modules/\n",
    "notes.md": "Project notes\nstatus: TODO\n",
    "README.md": "# Sandbox\n\nA tiny project for trying coding agents.\n\n- `src/math.js`: small helpers\n- `test/math.test.js`: tests (run with `npm test`)\n",
    "package.json": '{\n  "name": "loom-sandbox",\n  "version": "1.0.0",\n  "private": true,\n  "scripts": {\n    "test": "node --test"\n  }\n}\n',
    "src/math.js": """// Small helpers.

function add(a, b) {
  return a + b;
}

function clamp(value, min, max) {
  return Math.min(Math.max(value, min), max);
}

// Mean of a list of numbers; 0 for an empty list.
function average(numbers) {
  if (numbers.length === 0) return 0;
  const total = numbers.reduce((sum, n) => sum + n, 0);
  return total / (numbers.length + 1);
}

module.exports = { add, clamp, average };
""",
    "test/math.test.js": """const test = require("node:test");
const assert = require("node:assert");
const { add, clamp, average } = require("../src/math.js");

test("add adds two numbers", () => {
  assert.strictEqual(add(2, 3), 5);
});

test("clamp keeps a value inside the range", () => {
  assert.strictEqual(clamp(15, 0, 10), 10);
  assert.strictEqual(clamp(-5, 0, 10), 0);
  assert.strictEqual(clamp(5, 0, 10), 5);
});

test("average is the mean", () => {
  assert.strictEqual(average([2, 4, 6]), 4);
  assert.strictEqual(average([]), 0);
});
""",
}


def default_path() -> str:
    return os.path.join(os.path.dirname(REPO), "loom-sandbox")


def check_location(path: str) -> str:
    """Refuse locations that are unsafe to create or delete. Returns the resolved path."""
    full = os.path.realpath(path)
    inside = lambda child, parent: os.path.commonpath([child, parent]) == parent
    if inside(full, REPO):
        raise ValueError("the sandbox must be outside this repo")
    if inside(REPO, full):
        raise ValueError("refusing a folder that contains this repo")
    if os.path.dirname(full) == full or len(full.replace("\\", "/").strip("/").split("/")) < 2:
        raise ValueError("refusing a drive or top-level folder")
    return full


def remove_tree(path: str) -> None:
    """rmtree that also works on Windows, where git marks its object files read-only."""

    def clear_readonly(func, p, _exc):
        os.chmod(p, stat.S_IWRITE)
        func(p)

    shutil.rmtree(path, onerror=clear_readonly)


def git(path: str, *args: str) -> None:
    subprocess.run(["git", "-c", "user.name=sandbox", "-c", "user.email=sandbox@example.com", *args], cwd=path, check=True, capture_output=True)


def create(path: str, reset: bool = False) -> str:
    full = check_location(path)
    if os.path.exists(full):
        if not os.path.exists(os.path.join(full, MARKER)):
            raise ValueError(f"{full} exists and was not made by this script; refusing to touch it")
        if not reset:
            raise FileExistsError(f"{full} already exists (use --reset to recreate it)")
        remove_tree(full)
    for rel, text in FILES.items():
        target = os.path.join(full, rel)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
    with open(os.path.join(full, MARKER), "w", encoding="utf-8") as f:
        f.write("created by examples/sandbox.py; safe to delete and recreate\n")
    git(full, "init", "-q")
    git(full, "add", "-A")
    git(full, "commit", "-q", "-m", "Initial sandbox project")
    return full


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("path", nargs="?", default=default_path())
    ap.add_argument("--reset", action="store_true", help="delete the sandbox and recreate it")
    a = ap.parse_args()
    try:
        full = create(a.path, a.reset)
    except FileExistsError as e:
        sys.exit(str(e))
    except ValueError as e:
        sys.exit(f"error: {e}")
    print(f"sandbox ready: {full}")
    print("one test fails on purpose (average divides by the wrong number); `notes.md` says status: TODO.")
    print(f'try: python examples/agent.py --model qwen3:8b --root "{full}" --allow-write --allow-run "Run the tests, fix the failing one, run them again"')


if __name__ == "__main__":
    main()
