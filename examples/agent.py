#!/usr/bin/env python3
"""A small tool-using agent for exercising Loom with any OpenAI-compatible model.

It talks to Loom's OpenAI route, so every request shows up as a session in the Loom UI.
No third-party packages needed.

By default it is READ-ONLY. Tools, all confined to --root:

    list_dir, read_file, search            always on
    write_file, edit_file                  with --allow-write
    run_command                            with --allow-run (strict allowlist, see ALLOWED_COMMANDS)

Writing and running ask for your confirmation each time unless you pass --yes. Run write tasks on a
copy or a git branch you can throw away: the allowlist limits what can be run, it is not a sandbox
(for example `cargo test` runs whatever the project's tests do).

Example (Loom on :7070, Ollama on :11434):

    # terminal 1: Loom pointed at your model server
    $env:LOOM_OPENAI_UPSTREAM = "http://localhost:11434"; cargo run --release

    # terminal 2
    python examples/agent.py --model qwen3:8b "Explain how session ids are resolved"
    python examples/agent.py --model qwen3:8b --allow-write --allow-run "Add a unit test for glob_match and run the tests"
"""

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import urllib.error
import urllib.request

SKIP_DIRS = {".git", "node_modules", "target", "dist", "__pycache__"}
MAX_OUT = 6000  # characters returned to the model from one tool call
MAX_WRITE = 200_000  # bytes one write_file call may create
COMMAND_TIMEOUT = 120  # seconds

# The only commands run_command will execute: executable -> allowed argument prefixes.
# Everything is run without a shell, so ; | & > < ` $ ( ) cannot chain or redirect anything.
ALLOWED_COMMANDS = {
    "git": [["status"], ["log"], ["diff"], ["show"], ["branch"], ["ls-files"], ["rev-parse"]],
    "cargo": [["test"], ["check"], ["build"], ["clippy"], ["fmt", "--check"]],
    "npm": [["test"], ["run", "test"], ["run", "typecheck"], ["run", "build"]],
}
# Arguments that make an otherwise read-only command write files or run other programs.
FORBIDDEN_ARGS = ("--output", "--ext-diff", "--textconv", "--exec", "--upload-pack", "--receive-pack", "--git-dir", "--work-tree", "--fix")
# `git branch` may list branches but not create, move or delete them.
GIT_BRANCH_WRITE_FLAGS = {"-d", "-D", "-m", "-M", "-c", "-C", "-f", "-u", "--delete", "--move", "--copy", "--force", "--set-upstream-to", "--unset-upstream", "--edit-description"}
SHELL_CHARS = set(";|&><`$()\n\r{}")


def tool(name: str, description: str, properties: dict, required: list) -> dict:
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required},
        },
    }


def build_tools(allow_write: bool, allow_run: bool) -> list:
    tools = [
        tool("list_dir", "List files and folders in a directory (relative to the project root).",
             {"path": {"type": "string", "description": "Directory, e.g. '.' or 'crates/loom-core'"}}, ["path"]),
        tool("read_file", "Read a text file (relative to the project root). Long files are cut off.",
             {"path": {"type": "string", "description": "File path, e.g. 'Cargo.toml'"}}, ["path"]),
        tool("search", "Find lines containing a text snippet (case-insensitive) in files under the project root.",
             {"text": {"type": "string", "description": "Text to look for"}}, ["text"]),
    ]
    if allow_write:
        tools += [
            tool("write_file", "Create a file, or replace it completely. Prefer edit_file for small changes to an existing file.",
                 {"path": {"type": "string"}, "content": {"type": "string", "description": "The full new contents"}}, ["path", "content"]),
            tool("edit_file", "Replace one exact piece of text in an existing file. `old` must appear exactly once.",
                 {"path": {"type": "string"}, "old": {"type": "string", "description": "Exact text to replace"},
                  "new": {"type": "string", "description": "Replacement text"}}, ["path", "old", "new"]),
        ]
    if allow_run:
        allowed = "; ".join(f"{exe} {' '.join(p)}" for exe, prefixes in ALLOWED_COMMANDS.items() for p in prefixes)
        tools.append(tool("run_command", f"Run one command in the project root. Allowed: {allowed}. Extra arguments are fine.",
                          {"command": {"type": "string", "description": "e.g. 'cargo test' or 'git diff'"}}, ["command"]))
    return tools


# ------------------------------------------------------------------ safety rules (unit-tested)


def safe_path(root: str, rel: str) -> str:
    """Resolve `rel` inside `root`, refusing anything that escapes it (.., absolute paths, symlinks)."""
    full = os.path.realpath(os.path.join(root, rel))
    if os.path.commonpath([root, full]) != root:
        raise ValueError("path is outside the project root")
    return full


def check_writable(root: str, full: str) -> None:
    rel = os.path.relpath(full, root)
    if rel == "." or os.path.isdir(full):
        raise ValueError("that is a directory")
    if ".git" in rel.replace("\\", "/").split("/"):
        raise ValueError("refusing to write inside .git")


def parse_command(command: str) -> list:
    """Validate a run_command request and return its argv. Raises ValueError with the reason."""
    if any(c in SHELL_CHARS for c in command):
        raise ValueError("shell syntax (; | & > < ` $ ( ) { }) is not allowed: give a single plain command")
    try:
        argv = shlex.split(command)
    except ValueError as e:
        raise ValueError(f"cannot parse command: {e}")
    if not argv:
        raise ValueError("empty command")
    exe, args = argv[0], argv[1:]
    if exe not in ALLOWED_COMMANDS:
        raise ValueError(f"'{exe}' is not allowed. Allowed programs: {', '.join(ALLOWED_COMMANDS)}")
    if not any(args[: len(p)] == p for p in ALLOWED_COMMANDS[exe]):
        shown = "; ".join(" ".join([exe] + p) for p in ALLOWED_COMMANDS[exe])
        raise ValueError(f"'{' '.join(argv[:2])}' is not allowed. Allowed: {shown}")
    bad = next((a for a in args if a.startswith(FORBIDDEN_ARGS) or a == "-o"), None)
    if exe == "git" and args[0] == "branch":
        bad = bad or next((a for a in args[1:] if a in GIT_BRANCH_WRITE_FLAGS or not a.startswith("-")), None)  # any bare name would create a branch
    if bad:
        raise ValueError(f"argument '{bad}' is not allowed")
    return argv


# ------------------------------------------------------------------ tool implementations


def clip(text: str) -> str:
    return text[:MAX_OUT] + ("\n...[truncated]" if len(text) > MAX_OUT else "")


def run_tool(root: str, name: str, args: dict, allow_write: bool = False, allow_run: bool = False, confirm=lambda _: True) -> str:
    try:
        if name == "list_dir":
            p = safe_path(root, args.get("path", "."))
            entries = sorted(e for e in os.listdir(p) if e not in SKIP_DIRS)
            return "\n".join(e + ("/" if os.path.isdir(os.path.join(p, e)) else "") for e in entries) or "(empty)"
        if name == "read_file":
            with open(safe_path(root, args["path"]), encoding="utf-8", errors="replace") as f:
                return clip(f.read())
        if name == "search":
            needle, hits = args["text"].lower(), []
            for dirpath, dirs, files in os.walk(root):
                dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
                for fn in files:
                    path = os.path.join(dirpath, fn)
                    try:
                        with open(path, encoding="utf-8") as f:
                            for i, line in enumerate(f, 1):
                                if needle in line.lower():
                                    hits.append(f"{os.path.relpath(path, root)}:{i}: {line.strip()[:160]}")
                    except (UnicodeDecodeError, OSError):
                        continue
                    if len(hits) >= 40:
                        return "\n".join(hits) + "\n...[more matches omitted]"
            return "\n".join(hits) or "no matches"

        if name in ("write_file", "edit_file"):
            if not allow_write:
                return "error: writing is disabled (the user did not pass --allow-write)"
            full = safe_path(root, args["path"])
            check_writable(root, full)
            if name == "write_file":
                content = args["content"]
                if len(content.encode()) > MAX_WRITE:
                    return f"error: content is larger than {MAX_WRITE} bytes"
                if not confirm(f"write {len(content)} chars to {args['path']}" + (" (replaces the existing file)" if os.path.exists(full) else "")):
                    return "error: the user declined this write"
                os.makedirs(os.path.dirname(full), exist_ok=True)
                with open(full, "w", encoding="utf-8", newline="") as f:
                    f.write(content)
                return f"wrote {len(content)} chars to {args['path']}"
            with open(full, encoding="utf-8") as f:
                text = f.read()
            old, new = args["old"], args["new"]
            count = text.count(old)
            if old == "":
                return "error: `old` is empty"
            if count != 1:
                return f"error: `old` appears {count} times in {args['path']}; it must appear exactly once. Read the file and copy the text exactly."
            if not confirm(f"edit {args['path']}: replace {len(old)} chars with {len(new)} chars"):
                return "error: the user declined this edit"
            with open(full, "w", encoding="utf-8", newline="") as f:
                f.write(text.replace(old, new, 1))
            return f"edited {args['path']}"

        if name == "run_command":
            if not allow_run:
                return "error: running commands is disabled (the user did not pass --allow-run)"
            argv = parse_command(args["command"])
            exe = shutil.which(argv[0])
            if not exe:
                return f"error: {argv[0]} is not installed"
            if not confirm(f"run: {' '.join(argv)}"):
                return "error: the user declined this command"
            try:
                r = subprocess.run([exe] + argv[1:], cwd=root, capture_output=True, text=True, errors="replace",
                                   timeout=COMMAND_TIMEOUT, stdin=subprocess.DEVNULL)
            except subprocess.TimeoutExpired:
                return f"error: command timed out after {COMMAND_TIMEOUT} seconds"
            return clip(f"exit code {r.returncode}\n{r.stdout}{r.stderr}")
        return f"unknown tool {name}"
    except Exception as e:  # tool errors go back to the model, not the console
        return f"error: {e}"


# ------------------------------------------------------------------ talking to the model


def chat(base_url: str, model: str, session: str, messages: list, tools: list, extra: dict) -> dict:
    body = json.dumps({"model": model, "messages": messages, "tools": tools, "stream": False, **extra}).encode()
    req = urllib.request.Request(
        base_url.rstrip("/") + "/chat/completions",
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer " + os.environ.get("OPENAI_API_KEY", "ollama"),
            "x-loom-session": session,
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=600) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        sys.exit(f"HTTP {e.code} from {base_url}: {e.read().decode(errors='replace')[:500]}")
    except urllib.error.URLError as e:
        sys.exit(f"cannot reach {base_url}: {e.reason}")


def make_confirm(auto_yes: bool):
    def confirm(what: str) -> bool:
        print(f"      -> {what}")
        if auto_yes:
            return True
        if not sys.stdin.isatty():
            print("      (not an interactive terminal: declining; pass --yes to allow)")
            return False
        return input("      allow? [y/N] ").strip().lower() in ("y", "yes")

    return confirm


def system_prompt(allow_write: bool, allow_run: bool) -> str:
    p = "You are a careful coding assistant working in a project folder. Use the tools to look at files before answering. "
    if allow_write:
        p += "Read a file before you edit it, make the smallest change that does the job, and use edit_file for small changes. "
    if allow_run:
        p += "After changing code, run the project's tests or checks with run_command and fix what fails. "
    return p + "Answer concisely once you have what you need."


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("task", nargs="?", default="Explore this project and explain how session ids are resolved.")
    ap.add_argument("--base-url", default="http://localhost:7070/v1", help="Loom's OpenAI endpoint")
    ap.add_argument("--model", default="qwen3:8b")
    ap.add_argument("--session", default="qwen-demo", help="session name shown in the Loom UI")
    ap.add_argument("--root", default=".", help="project directory the tools may read (and write, with --allow-write)")
    ap.add_argument("--max-steps", type=int, default=15)
    ap.add_argument("--allow-write", action="store_true", help="enable write_file and edit_file")
    ap.add_argument("--allow-run", action="store_true", help="enable run_command (git, cargo and npm checks only)")
    ap.add_argument("--yes", action="store_true", help="do not ask before each write or command")
    ap.add_argument(
        "--reasoning",
        help="reasoning_effort to send (e.g. none, low). Default: none for Qwen3 models, whose thinking can take minutes on small GPUs; nothing otherwise.",
    )
    a = ap.parse_args()

    root = os.path.realpath(a.root)
    effort = a.reasoning or ("none" if "qwen3" in a.model.lower() else None)
    extra = {"reasoning_effort": effort} if effort else {}
    tools = build_tools(a.allow_write, a.allow_run)
    confirm = make_confirm(a.yes)
    messages = [
        {"role": "system", "content": system_prompt(a.allow_write, a.allow_run)},
        {"role": "user", "content": a.task},
    ]
    print(f"tools: {', '.join(t['function']['name'] for t in tools)}  |  root: {root}")

    for step in range(1, a.max_steps + 1):
        msg = chat(a.base_url, a.model, a.session, messages, tools, extra)["choices"][0]["message"]
        calls = msg.get("tool_calls") or []
        messages.append({k: v for k, v in msg.items() if k in ("role", "content", "tool_calls") and v is not None})
        if not calls:
            print(f"\n== answer (after {step} model call{'s' if step > 1 else ''}) ==\n{msg.get('content', '').strip()}")
            return
        for c in calls:
            fn = c["function"]
            try:
                args = json.loads(fn.get("arguments") or "{}")
            except json.JSONDecodeError:
                args = {}
            print(f"[{step}] {fn['name']}({json.dumps(args)[:200]})")
            result = run_tool(root, fn["name"], args, a.allow_write, a.allow_run, confirm)
            messages.append({"role": "tool", "tool_call_id": c["id"], "content": result})
    print(f"\nstopped after {a.max_steps} steps without a final answer")


if __name__ == "__main__":
    main()
