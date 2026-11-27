#!/usr/bin/env python3
"""Save a complete draft, restart, and send it through the native Backlog."""
import json
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

from desktop_check import ROOT, isolated_env, run_window


def check(executable, root, screenshots=None):
    checkout = root / "project"
    state = root / "state"
    subprocess.run(["git", "init", "-qb", "main", checkout], check=True)
    subprocess.run([
        "git", "-C", checkout, "-c", "user.name=Fixture",
        "-c", "user.email=fixture@example.test", "-c", "commit.gpgsign=false",
        "commit", "--allow-empty", "-qm", "Initial commit",
    ], check=True)
    attachment = root / "notes.txt"
    attachment.write_text("Preserve these original bytes.")
    env = isolated_env(root)
    env["HUGGLER_CODEX"] = str(ROOT / "tests/fixtures/context-provider.py")
    title = "Review the saved context"
    prompt = "$build-guide Review the attached notes and summarize the findings."
    marker = "⟦Attachment 1: notes.txt⟧"
    send = f"Send draft/{title}"
    card = f"Draft card/{title}"
    edit = f"Edit draft/{title}"
    prompt_preview = f"Draft prompt/{title}"
    title_field = f"Draft title/{title}"
    details = f"Draft details/{title}"
    details_panel = f"Draft details panel/{title}"
    def capture(name):
        if screenshots is None:
            return []
        return [{"screenshot": {"name": name, "path": str(screenshots / f"{name}.png")}}]

    appearances = [] if screenshots is None else [
        {"click": "Settings"}, {"click": "Appearance"}, {"click": "Light"},
        {"click": "Backlog"}, *capture("wide-light"),
        {"resize": [640, 600]}, {"same_line": [details, send]}, *capture("compact-light"),
        {"resize": [1120, 740]}, {"click": "Settings"}, {"click": "Appearance"},
        {"click": "Dark"}, {"click": "Backlog"}, *capture("wide-dark"),
    ]
    run_window(executable, state, [
        {"above": ["Settings", "Backlog"]}, {"click": "Backlog"},
        {"click": "Create a draft"}, {"key": "cmd-shift-n"},
        {"click": "Git checkout"}, {"type": str(checkout)},
        {"click": "Conversation name"}, {"type": title},
        {"click": "New worktree branch (optional)"}, {"type": "backlog-fixture"},
        {"click": "Back to message"},
        {"click": "model-automatic"}, {"click": "model-option/fixture-deep"},
        {"click": "model/fixture-deep"}, {"click": "Effort"}, {"click": "effort/high"},
        {"click": "Fast mode off"}, {"key": "escape"},
        {"click": "First message"}, {"type": "/build"},
        {"see": "suggestion//build-guide"}, {"key": "enter"},
        {"type": "Review the attached notes."},
        {"next_file_selection": [str(attachment)]}, {"click": "Attach files"},
        {"key": "cmd-a"}, {"key": "cmd-c"}, {"clipboard_contains": marker},
        *capture("composer"), {"click": "Save to Backlog"}, {"see": send},
        {"absent": f"thread/{title}"},
        {"above": [prompt_preview, edit]}, {"absent": title_field},
        {"absent": details_panel}, {"click": details}, {"see": details_panel},
        {"see": title_field},
        *capture("details"),
        {"click": details}, {"absent": details_panel},
        {"click": edit}, {"absent": send},
        {"above": ["Draft message", "Conversation title (optional)"]}, *capture("edit"),
        {"click": "Cancel draft edit"}, {"see": send}, {"click": edit},
        {"click": "Draft message"}, {"key": "cmd-a"}, {"type": prompt},
        {"click": "Save draft changes"}, {"see": card},
        {"click": "Create a draft"}, {"click": "First message"},
        {"type": "Discard this draft"}, {"click": "Save to Backlog"},
        *appearances,
        {"absent": "Draft details panel/Discard this draft"},
        {"see": "Delete draft/Discard this draft"},
        {"next_prompt_answer": 0}, {"click": "Delete draft/Discard this draft"},
        {"see": "Draft card/Discard this draft"},
        {"next_prompt_answer": 1}, {"click": "Delete draft/Discard this draft"},
        {"absent": "Draft card/Discard this draft"},
    ], env)
    attachment.unlink()
    draft_path, = (state / "backlog").glob("*.draft")
    saved_draft = draft_path.read_bytes()
    unavailable = checkout.with_name("moved")
    checkout.rename(unavailable)
    run_window(executable, state, [
        {"click": "Backlog"}, {"absent": details_panel},
        {"click": send}, {"contains": "Could not open"},
        {"see": send},
        *([{"resize": [960, 640]}, *capture("retry")] if screenshots else []),
    ], env)
    unavailable.rename(checkout)
    subprocess.run(["git", "init", "-qb", "main", root], check=True)
    git_directory = root / "project-git"
    (checkout / ".git").rename(git_directory)
    run_window(executable, state, [
        {"click": "Backlog"}, {"click": send},
        {"contains": "points to a different Git repository"}, {"see": send},
    ], env)
    git_directory.rename(checkout / ".git")

    # An invalid attachment name is rejected by enqueue after launch commits.
    header, payload = saved_draft.split(b"\n", 1)
    metadata = json.loads(header)
    metadata["files"][0]["name"] = "invalid/name.txt"
    draft_path.write_bytes(json.dumps(metadata).encode() + b"\n" + payload)
    run_window(executable, state, [
        {"click": "Backlog"}, {"click": send},
        {"contains": "Could not send draft"}, {"see": send},
    ], env)
    with sqlite3.connect(state / "huggler.sqlite3") as db:
        pending_thread, = db.execute("SELECT id FROM thread").fetchone()
        assert db.execute("SELECT count(*) FROM message WHERE role='user'").fetchone() == (0,)
    draft_path.write_bytes(saved_draft)
    run_window(executable, state, [
        {"click": "Backlog"}, {"click": send},
        {"see": f"Ready/{title}/Completed"},
        {"click": "Backlog"}, {"absent": send}, {"see": "Create a draft"},
        *capture("empty"),
    ], env)
    # Simulate an exit after the enqueue committed but before the draft was removed.
    assert not draft_path.exists()
    draft_path.write_bytes(saved_draft)
    run_window(executable, state, [
        {"click": "Backlog"}, {"click": send},
        {"see": f"Ready/{title}/Completed"},
        {"click": "Backlog"}, {"absent": send},
    ], env)
    with sqlite3.connect(state / "huggler.sqlite3") as db:
        assert db.execute("""
            SELECT name, provider_instance, model, model_effort, model_service_tier FROM thread
        """).fetchall() == [(title, "codex", "fixture-deep", "high", "priority")]
        assert db.execute("SELECT id FROM thread").fetchall() == [(pending_thread,)]
        text, = db.execute("SELECT text FROM message WHERE role='user'").fetchone()
        assert prompt + " " + marker in text and "Preserve these original bytes." in text
        roots = [Path(root) for root, in db.execute("SELECT root FROM project WHERE id IN (SELECT project FROM thread)")]
    worktree, = roots
    assert worktree != checkout.resolve()
    branch = subprocess.check_output(
        ["git", "-C", worktree, "branch", "--show-current"], text=True
    ).strip()
    assert branch == "backlog-fixture"
    frames = [json.loads(line) for line in (worktree / "frames.ndjson").read_text().splitlines()]
    turn, = (frame["params"] for frame in frames if frame.get("method") == "turn/start")
    assert turn["model"] == "fixture-deep"
    assert turn["effort"] == "high" and turn["serviceTierForTurn"] == "priority"
    assert prompt in turn["input"][0]["text"]
    assert "Preserve these original bytes." in turn["input"][0]["text"]
    assert not list((state / "backlog").glob("*.draft"))


if __name__ == "__main__":
    executable = Path(sys.argv[1]).resolve()
    screenshots = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else None
    with tempfile.TemporaryDirectory(prefix="huggler backlog ") as temporary:
        check(executable, Path(temporary), screenshots)
