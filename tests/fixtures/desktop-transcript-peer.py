#!/usr/bin/env python3
"""Script a mixed transcript over Codex app-server stdio."""
import json
import os
import sys
import time

assert sys.argv[1:] == ["app-server"]
session = "transcript-fixture"
turn = "review"

def emit(**frame):
    print(json.dumps(frame), flush=True)

def complete(item_id, kind, **fields):
    emit(method="item/completed", params={
        "threadId": session, "turnId": turn,
        "item": {"id": item_id, "type": kind, **fields},
    })

for line in sys.stdin:
    frame = json.loads(line)
    if "id" not in frame:
        continue
    method = frame.get("method")
    if method in ("thread/start", "thread/resume"):
        result = {"thread": {"id": session}}
    elif method == "model/list":
        result = {"data": []}
    elif method == "turn/start":
        result = {"turn": {"id": turn}}
    else:
        result = {}
    emit(id=frame["id"], result=result)
    if method != "turn/start":
        continue
    if os.environ.get("HUGGLER_TRACE_LIST_FIXTURE") == "1":
        for index in range(55):
            complete(f"trace-{index}", "commandExecution", command=f"fixture command {index}",
                     status="completed", aggregatedOutput=f"Trace result {index}")
        emit(method="turn/completed", params={"threadId": session, "turn": {"id": turn, "status": "completed"}})
        continue
    complete("intro", "agentMessage", phase="commentary",
             text="I’ll start with the conversation layout.")
    complete("comment", "agentMessage", phase="commentary",
             text="I’ll check the transcript layout and run the native workflow checks.")
    complete("command", "commandExecution", command="cargo test --locked --test context",
             status="completed", aggregatedOutput="All 12 checks passed.")
    complete("lookup", "mcpToolCall", server="docs", tool="lookup", status="failed",
             arguments={"query": "Review notes"}, error={"message": "No matching document"})
    time.sleep(8)
    complete("compact", "contextCompaction")
    complete("patch", "fileChange", status="completed", changes=[{"path": "src/conversation.rs"}])
    complete("reply", "agentMessage", phase="final_answer",
             text="## Review summary\n\nThe transcript keeps the focus on the conversation. Tool activity "
                  "is grouped by turn, and attachments open inline.\n\n"
                  "- The native workflow checks passed.\n"
                  "- Saved command output is available after reopening.")
    emit(method="turn/completed", params={"threadId": session, "turn": {"id": turn, "status": "completed"}})
