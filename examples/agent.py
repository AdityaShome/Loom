#!/usr/bin/env python3
"""A tiny tool-using agent for exercising Loom with any OpenAI-compatible model.

It talks to Loom's OpenAI route, so every request shows up as a session in the
Loom UI. No third-party packages needed.

Tools are read-only and confined to --root: list_dir, read_file, search.

Example (Loom on :7070, Ollama on :11434):

    # terminal 1
    set LOOM_OPENAI_UPSTREAM=http://localhost:11434   (PowerShell: $env:LOOM_OPENAI_UPSTREAM=...)
    cargo run --release

    # terminal 2
    python examples/agent.py --model qwen3:4b "Explain how session ids are resolved"
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "list_dir",
            "description": "List files and folders in a directory (relative to the project root).",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string", "description": "Directory, e.g. '.' or 'crates/loom-core'"}},
                "required": ["path"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read a text file (relative to the project root).",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string", "description": "File path, e.g. 'Cargo.toml'"}},
                "required": ["path"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "search",
            "description": "Find lines containing a text snippet (case-insensitive) in files under the project root.",
            "parameters": {
                "type": "object",
                "properties": {"text": {"type": "string", "description": "Text to look for"}},
                "required": ["text"],
            },
        },
    },
]

SKIP_DIRS = {".git", "node_modules", "target", "dist", "__pycache__"}
MAX_OUT = 6000


def safe_path(root: str, rel: str) -> str:
    full = os.path.realpath(os.path.join(root, rel))
    if os.path.commonpath([root, full]) != root:
        raise ValueError("path is outside the project root")
    return full


def run_tool(root: str, name: str, args: dict) -> str:
    try:
        if name == "list_dir":
            p = safe_path(root, args.get("path", "."))
            entries = sorted(e for e in os.listdir(p) if e not in SKIP_DIRS)
            return "\n".join(e + ("/" if os.path.isdir(os.path.join(p, e)) else "") for e in entries) or "(empty)"
        if name == "read_file":
            with open(safe_path(root, args["path"]), encoding="utf-8", errors="replace") as f:
                text = f.read()
            return text[:MAX_OUT] + ("\n...[truncated]" if len(text) > MAX_OUT else "")
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
        return f"unknown tool {name}"
    except Exception as e:  # tool errors go back to the model, not the console
        return f"error: {e}"


def chat(base_url: str, model: str, session: str, messages: list) -> dict:
    body = json.dumps({"model": model, "messages": messages, "tools": TOOLS, "stream": False}).encode()
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


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("task", nargs="?", default="Explore this project and explain how session ids are resolved.")
    ap.add_argument("--base-url", default="http://localhost:7070/v1", help="Loom's OpenAI endpoint")
    ap.add_argument("--model", default="qwen3:4b")
    ap.add_argument("--session", default="qwen-demo", help="session name shown in the Loom UI")
    ap.add_argument("--root", default=".", help="project directory the tools may read")
    ap.add_argument("--max-steps", type=int, default=15)
    a = ap.parse_args()

    root = os.path.realpath(a.root)
    messages = [
        {
            "role": "system",
            "content": "You are a careful code-reading assistant. Use the tools to look at files before answering. "
            "Answer concisely once you have what you need.",
        },
        {"role": "user", "content": a.task},
    ]

    for step in range(1, a.max_steps + 1):
        msg = chat(a.base_url, a.model, a.session, messages)["choices"][0]["message"]
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
            print(f"[{step}] {fn['name']}({json.dumps(args)})")
            messages.append({"role": "tool", "tool_call_id": c["id"], "content": run_tool(root, fn["name"], args)})
    print(f"\nstopped after {a.max_steps} steps without a final answer")


if __name__ == "__main__":
    main()
