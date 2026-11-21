"""Tests for the safety rules of examples/agent.py. No dependencies: python examples/test_agent.py"""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("agent", os.path.join(HERE, "agent.py"))
agent = importlib.util.module_from_spec(spec)
spec.loader.exec_module(agent)
sb_spec = importlib.util.spec_from_file_location("sandbox", os.path.join(HERE, "sandbox.py"))
sandbox = importlib.util.module_from_spec(sb_spec)
sb_spec.loader.exec_module(sandbox)


def names(tools):
    return [t["function"]["name"] for t in tools]


class Sandbox(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = os.path.realpath(self._tmp.name)
        self.outside = os.path.realpath(tempfile.mkdtemp())
        with open(os.path.join(self.root, "a.txt"), "w", encoding="utf-8") as f:
            f.write("hello world\nsecond line\n")
        os.makedirs(os.path.join(self.root, "sub"))

    def tearDown(self):
        self._tmp.cleanup()
        shutil.rmtree(self.outside, ignore_errors=True)

    def run_tool(self, name, args, write=False, run=False, confirm=lambda _: True):
        return agent.run_tool(self.root, name, args, write, run, confirm)


class Tools(Sandbox):
    def test_default_is_read_only(self):
        self.assertEqual(names(agent.build_tools(False, False)), ["list_dir", "read_file", "search"])
        self.assertEqual(names(agent.build_tools(True, False))[3:], ["write_file", "edit_file"])
        self.assertEqual(names(agent.build_tools(False, True))[3:], ["run_command"])
        self.assertEqual(len(agent.build_tools(True, True)), 6)

    def test_read_tools(self):
        self.assertIn("a.txt", self.run_tool("list_dir", {"path": "."}))
        self.assertIn("sub/", self.run_tool("list_dir", {"path": "."}))
        self.assertIn("second line", self.run_tool("read_file", {"path": "a.txt"}))
        self.assertIn("a.txt:2:", self.run_tool("search", {"text": "SECOND"}))
        self.assertEqual(self.run_tool("search", {"text": "zzz-not-there"}), "no matches")

    def test_long_files_are_cut_off(self):
        with open(os.path.join(self.root, "big.txt"), "w", encoding="utf-8") as f:
            f.write("x" * 20000)
        out = self.run_tool("read_file", {"path": "big.txt"})
        self.assertLess(len(out), 6100)
        self.assertTrue(out.endswith("[truncated]"))

    def test_paths_cannot_escape_the_root(self):
        for bad in ["..", "../x", os.path.join("sub", "..", "..", "x"), self.outside, os.path.join(self.outside, "f.txt")]:
            out = self.run_tool("read_file", {"path": bad})
            self.assertIn("outside the project root", out, bad)
        self.assertIn("outside the project root", self.run_tool("list_dir", {"path": ".."}))

    def test_symlink_out_of_the_root_is_refused(self):
        link = os.path.join(self.root, "link")
        try:
            os.symlink(self.outside, link, target_is_directory=True)
        except (OSError, NotImplementedError):
            self.skipTest("symlinks not available here")
        self.assertIn("outside the project root", self.run_tool("list_dir", {"path": "link"}))


class Writing(Sandbox):
    def test_disabled_without_the_flag(self):
        for name, args in [("write_file", {"path": "n.txt", "content": "x"}), ("edit_file", {"path": "a.txt", "old": "hello", "new": "bye"})]:
            self.assertIn("disabled", self.run_tool(name, args))
        self.assertFalse(os.path.exists(os.path.join(self.root, "n.txt")))
        self.assertIn("hello", self.run_tool("read_file", {"path": "a.txt"}))

    def test_write_creates_files_and_folders(self):
        out = self.run_tool("write_file", {"path": "new/dir/f.txt", "content": "héllo"}, write=True)
        self.assertIn("wrote 5 chars", out)
        with open(os.path.join(self.root, "new", "dir", "f.txt"), encoding="utf-8") as f:
            self.assertEqual(f.read(), "héllo")

    def test_write_outside_root_git_dir_and_directories_are_refused(self):
        for path in ["../evil.txt", os.path.join(self.outside, "evil.txt"), ".git/config", "sub/.git/hooks/pre-commit", "sub", "."]:
            out = self.run_tool("write_file", {"path": path, "content": "x"}, write=True)
            self.assertTrue(out.startswith("error:"), f"{path} -> {out}")
        self.assertFalse(os.path.exists(os.path.join(self.outside, "evil.txt")))

    def test_oversized_write_is_refused(self):
        out = self.run_tool("write_file", {"path": "big.txt", "content": "x" * (agent.MAX_WRITE + 1)}, write=True)
        self.assertIn("larger than", out)
        self.assertFalse(os.path.exists(os.path.join(self.root, "big.txt")))

    def test_edit_replaces_exactly_one_occurrence(self):
        self.assertEqual(self.run_tool("edit_file", {"path": "a.txt", "old": "hello", "new": "bye"}, write=True), "edited a.txt")
        self.assertIn("bye world", self.run_tool("read_file", {"path": "a.txt"}))

    def test_edit_refuses_missing_ambiguous_and_empty_matches(self):
        with open(os.path.join(self.root, "dup.txt"), "w", encoding="utf-8") as f:
            f.write("a a a")
        self.assertIn("appears 0 times", self.run_tool("edit_file", {"path": "a.txt", "old": "nope", "new": "x"}, write=True))
        self.assertIn("appears 3 times", self.run_tool("edit_file", {"path": "dup.txt", "old": "a", "new": "b"}, write=True))
        self.assertIn("empty", self.run_tool("edit_file", {"path": "a.txt", "old": "", "new": "x"}, write=True))
        with open(os.path.join(self.root, "dup.txt"), encoding="utf-8") as f:
            self.assertEqual(f.read(), "a a a", "a refused edit must not change the file")

    def test_declined_writes_change_nothing(self):
        out = self.run_tool("write_file", {"path": "n.txt", "content": "x"}, write=True, confirm=lambda _: False)
        self.assertIn("declined", out)
        out = self.run_tool("edit_file", {"path": "a.txt", "old": "hello", "new": "bye"}, write=True, confirm=lambda _: False)
        self.assertIn("declined", out)
        self.assertFalse(os.path.exists(os.path.join(self.root, "n.txt")))
        self.assertIn("hello", self.run_tool("read_file", {"path": "a.txt"}))

    def test_confirmation_describes_the_action(self):
        seen = []
        self.run_tool("write_file", {"path": "a.txt", "content": "zz"}, write=True, confirm=lambda w: seen.append(w) or True)
        self.assertIn("replaces the existing file", seen[0])


class Commands(Sandbox):
    def rejected(self, command):
        with self.assertRaises(ValueError, msg=command):
            agent.parse_command(command)

    def test_allowed_commands_parse(self):
        self.assertEqual(agent.parse_command("git status"), ["git", "status"])
        self.assertEqual(agent.parse_command("git log --oneline -5"), ["git", "log", "--oneline", "-5"])
        self.assertEqual(agent.parse_command("cargo test -p loom-core"), ["cargo", "test", "-p", "loom-core"])
        self.assertEqual(agent.parse_command("cargo fmt --check"), ["cargo", "fmt", "--check"])
        self.assertEqual(agent.parse_command("npm run typecheck"), ["npm", "run", "typecheck"])
        self.assertEqual(agent.parse_command("git branch --show-current"), ["git", "branch", "--show-current"])
        self.assertEqual(agent.parse_command("git branch -a"), ["git", "branch", "-a"])

    def test_other_programs_are_refused(self):
        for c in ["rm -rf .", "curl http://example.com", "python -c 'print(1)'", "powershell Get-ChildItem", "sh -c ls", "del a.txt", "node x.js", ""]:
            self.rejected(c)

    def test_destructive_subcommands_are_refused(self):
        for c in [
            "git push", "git commit -am x", "git reset --hard", "git checkout .", "git clean -fd", "git rm a.txt", "git config user.name x",
            "git -c core.pager=evil status", "git branch -D main", "git branch newbranch", "git branch -m old new", "git branch -a -D main",
            "cargo publish", "cargo install x", "cargo run", "cargo fmt", "cargo clippy --fix", "cargo clean",
            "npm install evil", "npm publish", "npm run deploy", "npm exec evil",
        ]:
            self.rejected(c)

    def test_flags_that_write_files_or_run_programs_are_refused(self):
        for c in ["git diff --output=x.txt", "git log --ext-diff", "git diff --textconv", "git show -o x", "git status --git-dir=/tmp", "git diff --exec=evil"]:
            self.rejected(c)

    def test_shell_syntax_is_refused(self):
        for c in ["git status; rm -rf .", "git status && rm a", "git status | cat", "git status > out.txt", "git status `whoami`",
                  "git status $(whoami)", "git status\nrm a", "cargo test || true", "git log --format=%H > f"]:
            self.rejected(c)

    def test_run_command_is_disabled_without_the_flag(self):
        self.assertIn("disabled", self.run_tool("run_command", {"command": "git status"}))

    def test_declined_command_is_not_run(self):
        out = self.run_tool("run_command", {"command": "git status"}, run=True, confirm=lambda _: False)
        self.assertIn("declined", out)

    def test_rejected_command_reports_why_to_the_model(self):
        out = self.run_tool("run_command", {"command": "git push"}, run=True)
        self.assertTrue(out.startswith("error:") and "not allowed" in out, out)

    @unittest.skipUnless(shutil.which("git"), "git is not installed")
    def test_allowed_command_runs_in_the_root_and_reports_the_exit_code(self):
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        out = self.run_tool("run_command", {"command": "git status --short"}, run=True)
        self.assertTrue(out.startswith("exit code 0"), out)
        self.assertIn("a.txt", out)  # untracked file listed, so it really ran in the project root
        out = self.run_tool("run_command", {"command": "git log"}, run=True)  # no commits yet: non-zero, reported not raised
        self.assertTrue(out.startswith("exit code"), out)
        self.assertNotIn("exit code 0", out.splitlines()[0])


class SandboxScript(unittest.TestCase):
    def setUp(self):
        self.base = tempfile.mkdtemp()
        self.path = os.path.join(self.base, "box")

    def tearDown(self):
        sandbox.remove_tree(self.base)

    @unittest.skipUnless(shutil.which("git"), "git is not installed")
    def test_creates_a_project_with_history_and_a_marker(self):
        full = sandbox.create(self.path)
        for rel in ["notes.md", "package.json", "src/math.js", "test/math.test.js", ".loom-sandbox"]:
            self.assertTrue(os.path.exists(os.path.join(full, rel)), rel)
        log = subprocess.run(["git", "log", "--oneline"], cwd=full, capture_output=True, text=True).stdout
        self.assertIn("Initial sandbox project", log)
        status = subprocess.run(["git", "status", "--short"], cwd=full, capture_output=True, text=True).stdout
        self.assertEqual(status.strip(), "", "starts clean, so every later change is the agent's")

    @unittest.skipUnless(shutil.which("git"), "git is not installed")
    def test_reset_restores_the_original_files(self):
        full = sandbox.create(self.path)
        with open(os.path.join(full, "notes.md"), "w", encoding="utf-8") as f:
            f.write("changed by an agent")
        with self.assertRaises(FileExistsError):
            sandbox.create(self.path)
        sandbox.create(self.path, reset=True)
        with open(os.path.join(full, "notes.md"), encoding="utf-8") as f:
            self.assertIn("status: TODO", f.read())

    def test_never_deletes_a_folder_it_did_not_create(self):
        os.makedirs(self.path)
        precious = os.path.join(self.path, "precious.txt")
        with open(precious, "w", encoding="utf-8") as f:
            f.write("keep me")
        for reset in (False, True):
            with self.assertRaises(ValueError):
                sandbox.create(self.path, reset=reset)
        self.assertTrue(os.path.exists(precious))

    def test_refuses_the_repo_its_children_its_parents_and_drive_roots(self):
        repo = sandbox.REPO
        for bad in [repo, os.path.join(repo, "sandbox"), os.path.join(repo, "examples"), os.path.dirname(repo), os.path.abspath(os.sep)]:
            with self.assertRaises(ValueError, msg=bad):
                sandbox.check_location(bad)

    def test_default_location_is_next_to_the_repo_not_inside_it(self):
        d = sandbox.check_location(sandbox.default_path())
        self.assertEqual(os.path.dirname(d), os.path.dirname(sandbox.REPO))
        self.assertNotEqual(d, sandbox.REPO)

    @unittest.skipUnless(shutil.which("git") and shutil.which("node"), "needs git and node")
    def test_the_sample_project_really_has_one_failing_test(self):
        full = sandbox.create(self.path)
        r = subprocess.run(["node", "--test"], cwd=full, capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("average", r.stdout + r.stderr)
        # the fix the agent should find
        p = os.path.join(full, "src", "math.js")
        with open(p, encoding="utf-8") as f:
            fixed = f.read().replace("numbers.length + 1", "numbers.length")
        with open(p, "w", encoding="utf-8") as f:
            f.write(fixed)
        self.assertEqual(subprocess.run(["node", "--test"], cwd=full, capture_output=True, text=True).returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
