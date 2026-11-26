#!/usr/bin/env python3
"""Deterministic app-server peer for the desktop edit, test and commit scenario."""
import json
from pathlib import Path
import subprocess
import shlex
import sys
import time
from uuid import uuid4

file = Path("answer [v1].txt")
full_access_policy = {"granular": {
    "mcp_elicitations": True, "request_permissions": False, "rules": False,
    "sandbox_approval": False, "skill_approval": False,
}}


def emit(**frame):
    print(json.dumps(frame), flush=True)


def notify(method, **params):
    emit(method=method, params=dict(threadId=session, turnId=turn, **params))


def complete(text, kind="agentMessage"):
    notify("item/completed", item={"type": kind, "id": turn + "-result", "text": text})
    emit(method="turn/completed", params={"threadId": session, "turn": {"id": turn, "status": "completed"}})


assert sys.argv[1:] == ["app-server"]
for line in sys.stdin:
    frame = json.loads(line)
    params = frame.get("params", {})
    method = frame.get("method")
    if "id" not in frame:
        continue
    if frame["id"] == "tool-approval":
        assert frame["result"] == {"action": "accept", "content": {}}
        complete("Full access confirmed.")
    elif frame["id"] == "confirm":
        assert frame["result"] == {"answers": {"filename": {"answers": [file.name]}}}
        emit(id="write", method="item/fileChange/requestApproval",
             params={"threadId": session, "turnId": turn, "itemId": "edit", "reason": "Create the answer file"})
    elif frame["id"] == "write":
        assert frame["result"] == {"decision": "accept"}
        file.write_text("41\n")
        complete("Created the answer file.")
    elif method in ("thread/start", "thread/resume"):
        assert Path(params["cwd"]) == Path.cwd()
        assert Path(subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"], text=True).rstrip("\n")) == Path.cwd()
        assert not params.get("ephemeral", False)
        approval_policy = params["approvalPolicy"]
        assert approval_policy in ("on-request", "never", full_access_policy)
        if approval_policy == full_access_policy:
            server_name, = [
                key.removeprefix("mcp_servers.") for key in params["config"]
                if key.startswith("mcp_servers.")
            ]
        session = params["threadId"] if method == "thread/resume" else str(uuid4())
        sandbox = params["sandbox"]
        emit(id=frame["id"], result={"thread": {"id": session}, "model": "fixture-model"})
    elif method == "turn/start":
        prompt = params["input"][0]["text"]
        if prompt.startswith("/create-pr\n"):
            assert 'Text attachment "PR context.txt"' in prompt
            assert "## Merge Danger" not in prompt
            inspect, publish = [
                shlex.split(block.split("\n", 1)[1].strip())
                for block in prompt.split("```") if block.startswith("sh\n")
            ]
            assert inspect[1] == "--inspect-pull-request"
            assert publish[1] == "--publish-pull-request"
            turn = str(uuid4())
            emit(id=frame["id"], result={"turn": {"id": turn}})
            inspected = subprocess.run(inspect, capture_output=True, text=True)
            if inspected.returncode:
                complete(inspected.stderr.strip())
                continue
            snapshot = json.loads(inspected.stdout)
            assert "name: create-pr" in snapshot["skill"]
            assert "## Merge Danger" in snapshot["skill"]
            root = Path.cwd().parent
            scenario_file = root / "commit-message.json"
            scenario = json.loads(scenario_file.read_text()) if scenario_file.exists() else "success"
            expected = root / "create.json"
            fields = json.loads(expected.read_text())["payload"] if expected.exists() else {
                "title": "Update review fixtures",
                "body": "Cover staged, unstaged, new, and removed files.",
            }
            payload = {
                "head_oid": snapshot["head_oid"],
                "pending_tree": snapshot["pending_tree"],
                "title": fields["title"],
                "body": fields["body"] or "Verification details for this fixture.",
                "commit_subject": "Commit the reviewed fixtures" if snapshot["pending_tree"] else None,
            }
            if scenario == "invalid":
                payload["commit_subject"] = "not\na single line"
            if scenario == "edit":
                Path("new.txt").write_text("New content, edited during generation.\n")
            published = subprocess.run(publish, input=json.dumps(payload), capture_output=True, text=True)
            complete((published.stderr if published.returncode else published.stdout).strip())
            continue
        if prompt == "Finish the pending review changes.":
            turn = str(uuid4())
            emit(id=frame["id"], result={"turn": {"id": turn}})
            notify("item/agentMessage/delta", delta="Preparing the final review change.")
            release = Path.cwd().parent / "finish-review"
            deadline = time.monotonic() + 20
            while not release.exists():
                assert time.monotonic() < deadline, "Queued PR was not observed"
                time.sleep(.05)
            Path("new.txt").write_text("Final content from the running turn.\n")
            complete("Final review change ready.")
            continue
        if params["collaborationMode"]["mode"] == "plan":
            assert prompt == "Plan the answer file."
            assert sandbox == "read-only" and params["sandboxPolicy"] == {"type": "readOnly"}
            turn = "plan"
            emit(id=frame["id"], result={"turn": {"id": turn}})
            complete("Create the answer file and check its value.", "plan")
            continue
        assert params["approvalPolicy"] == approval_policy
        if approval_policy == full_access_policy:
            assert sandbox == "danger-full-access"
            assert params["sandboxPolicy"] == {"type": "dangerFullAccess"}
        else:
            assert sandbox == "workspace-write"
            assert params["sandboxPolicy"] == {"type": "workspaceWrite", "networkAccess": False}
        if prompt in ("Check the default permissions.", "Check the saved permissions."):
            is_default = prompt == "Check the default permissions."
            assert approval_policy == (full_access_policy if is_default else "on-request")
            turn = str(uuid4())
            emit(id=frame["id"], result={"turn": {"id": turn}})
            if is_default:
                emit(id="tool-approval", method="mcpServer/elicitation/request", params={
                    "threadId": session, "turnId": turn, "serverName": server_name, "mode": "form",
                    "_meta": {"codex_approval_kind": "mcp_tool_call"},
                })
            else:
                complete("Approval mode confirmed.")
            continue
        if prompt.startswith(("Fix the failed checks", "Resolve the")):
            if prompt.startswith("Fix"):
                assert "fixture/project #12 (https://github.com/fixture/project/pull/12)" in prompt
            turn = str(uuid4())
            emit(id=frame["id"], result={"turn": {"id": turn}})
            complete("Received the pull request task.")
            continue
        turn = "revise" if file.exists() else "implement"
        emit(id=frame["id"], result={"turn": {"id": turn}})
        if prompt == ("Create the answer file. [fixture-tree](fixture-tree) "
                      "[linked-entry](fixture-tree/linked-entry) "):
            assert approval_policy == "on-request"
            text = ("Checking the requested filename. I will confirm the path with you before "
                    "creating the answer file in this checkout.")
            for character in text:
                notify("item/agentMessage/delta", delta=character)
            notify("item/started", item={"id": "inspect", "type": "commandExecution",
                                         "command": "git status --short", "status": "inProgress"})
            notify("item/commandExecution/outputDelta", itemId="inspect", delta="Checkout ready")
            emit(id="confirm", method="item/tool/requestUserInput", params={
                "threadId": session, "turnId": turn, "questions": [
                    {"id": "filename", "question": "Which file should contain the answer?"}]})
        elif prompt == "Wait for my next instruction.":
            notify("item/agentMessage/delta", delta="Waiting for your instruction.")
        elif prompt == "Review the staged result.":
            complete("Reviewed the staged result.")
        elif prompt == "Please start from default [base-only.txt](base-only.txt) ":
            assert Path(".git").is_file() and Path("base-only.txt").is_file() and not file.exists()
            complete("Started from the default branch.")
        elif prompt == "Watch for the review signal.":
            deadline = time.monotonic() + 30
            while not Path(".git/huggler-review-signal").exists():
                assert time.monotonic() < deadline, "Review signal timed out"
                time.sleep(.05)
            emit(method="turn/completed", params={"threadId": session, "turn": {
                "id": turn, "status": "failed", "error": {"message": "Review check failed"}}})
        else:
            assert prompt == "Use a single line."
            assert file.read_text() == "41\n"
            file.write_text("42\n")
            complete("Corrected the answer.")
    else:
        emit(id=frame["id"], result={})
