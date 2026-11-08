"""Deterministic ACP agent for delegation tests; no provider or network access."""

import json
import pathlib
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

output_lock = threading.Lock()
cancel = threading.Event()
permission = threading.Event()
permission_reply = None
servers = []


def send(value):
    with output_lock:
        print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


def update(value):
    send({"method": "session/update", "params": {"sessionId": "session", "update": value}})


def mcp_call(server_name, tool, arguments):
    """通过真实 MCP 进程调用工具；读取回复不需要接收者身份参数。"""
    server = next(s for s in servers if s["name"] == server_name)
    with subprocess.Popen([server["command"], *server["args"]], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, text=True) as mcp:
        def rpc(identifier, method, params):
            mcp.stdin.write(json.dumps({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}) + "\n")
            mcp.stdin.flush()
            return json.loads(mcp.stdout.readline())

        initialized = rpc(1, "initialize", {"protocolVersion": "2025-06-18"})
        assert initialized["result"]["serverInfo"]["name"] == "codync-" + server_name
        tools = rpc(2, "tools/list", {})
        assert any(t["name"] == tool for t in tools["result"]["tools"])
        next_id = 3
        if server_name == "team":
            assert {t["name"] for t in tools["result"]["tools"]} == {"list_bots", "message_bot", "read_bot_reply"}
            if tool == "message_bot":
                cursor = None
                while True:
                    # 实际沙箱可能超过默认二十个 bot，沿已有分页查找目标，不能假设第一页就是全集。
                    roster = rpc(next_id, "tools/call", {"name": "list_bots", "arguments": {"cursor": cursor} if cursor else {}})
                    next_id += 1
                    page = json.loads(roster["result"]["content"][0]["text"])
                    if any(b["id"] == arguments["botId"] for b in page["bots"]):
                        break
                    cursor = page.get("next")
                    assert cursor is not None, "recipient absent from all roster pages"
        response = rpc(next_id, "tools/call", {"name": tool, "arguments": arguments})
        mcp.stdin.close()
        return response["result"]


def team_call(target, message):
    return mcp_call("team", "message_bot", {"botId": target, "message": message})


def chain():
    """Hands the chain on to the bot in handoff.json; what the turn replies."""
    response = team_call(json.loads(pathlib.Path("handoff.json").read_text())["target"], "CHAIN")
    return "Chain stopped: " + response["content"][0]["text"] if response.get("isError") else "Chain forwarded"


REPLIES_END = "\n\n[Codync: these are bot replies"


def prompt(request):
    """执行委派、分页读取与取消测试场景，不访问真实模型或账户。"""
    text = request["params"]["prompt"][0]["text"]
    with open("prompts.jsonl", "a") as log:
        log.write(json.dumps(text) + "\n")
    if REPLIES_END in text:
        # Replies to requests this bot sent: relay what they said (whatever words they carry),
        # or pass on a chain the peer handed back.
        parts = text.split(REPLIES_END, 1)[0].split("\n\n")
        relayed = [p.split(".]\n", 1)[1] if ".]\n" in p else "failed" for p in parts]
        reply = chain() if any("CHAIN" in r for r in relayed) else "Relayed: " + " | ".join(relayed)
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        # A text update can reach sync before the host handles turn completion.
        # Keep that interval deterministic when the bounded chain stops.
        if reply.startswith("Chain stopped"):
            time.sleep(0.3)
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.startswith(("DELEGATE ", "DELEGATE_REPORT ")):
        # DELEGATE <target> [message], on the first line.
        target, *given = text.split("\n", 1)[0].split(" ", 2)[1:]
        update({"sessionUpdate": "tool_call", "toolCallId": "delegate", "title": "Message reviewer", "status": "in_progress"})
        message = "REPORT" if text.startswith("DELEGATE_REPORT ") else (given or ["PERMISSION review the changes"])[0]
        response = team_call(target, message)
        assert not response.get("isError"), response
        result = json.loads(response["content"][0]["text"])
        assert result["status"] == "queued" and "reply" not in result
        update({"sessionUpdate": "tool_call_update", "toolCallId": "delegate", "status": "completed"})
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Message queued"}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.startswith("READ_REPLY "):
        # 从模型实际可见的 MCP 文本读取，不能只验证 HTTP JSON 存在。
        arguments = json.loads(text.split("\n", 1)[0].removeprefix("READ_REPLY "))
        result = mcp_call("team", "read_bot_reply", arguments)
        reply = json.dumps(result) if result.get("isError") else result["content"][0]["text"]
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.split("\n\n")[-1] == "LONG_REPLY":
        reply = "结论：已完成\n" + "甲🙂" * 2200 + "\n中间证据\n" + "乙" * 2200 + "\n未验证：真实提供方费用"
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn", "usage": {"inputTokens": 123, "outputTokens": 456, "cachedReadTokens": 0}}})
        return
    if text.startswith("TRY "):
        # TRY <target> <message>: a message that may be refused; the reply says which.
        target, message = text.split("\n", 1)[0].split(" ", 2)[1:]
        response = team_call(target, message)
        reply = ("Refused: " if response.get("isError") else "Sent: ") + response["content"][0]["text"]
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.split("\n\n")[-1] == "REPORT":
        response = mcp_call("chat", "send_message", {"text": "Report for the user"})
        if "Your final reply goes back to" in text:
            assert response.get("isError"), response
            assert "requesting bot" in response["content"][0]["text"]
            reply = "Answer for the requesting bot"
        else:
            assert not response.get("isError"), response
            response = mcp_call("chat", "send_message", {"text": "Second report for the user"})
            assert not response.get("isError"), response
            reply = "Private completion trace"
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.split("\n\n")[-1] == "CHAIN":
        reply = chain()
        if "Your final reply goes back to" in text:
            # A request turn messages no one: it hands the chain back in its reply.
            assert "answering another bot's request" in reply, reply
            reply = "CHAIN back"
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.split("\n\n")[-1].startswith(("MANAGE ", "CHAT ")):
        helper, tool, arguments = text.split("\n\n")[-1].split(" ", 2)
        response = mcp_call(helper.lower(), tool, json.loads(arguments))
        reply = ("Refused: " if response.get("isError") else "Done: ") + response["content"][0]["text"]
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.split("\n\n")[-1].startswith("CHAT_TWICE "):
        # Two calls at once, as an agent's parallel tool calls are.
        _, tool, arguments = text.split("\n\n")[-1].split(" ", 2)
        with ThreadPoolExecutor(2) as pool:
            responses = pool.map(lambda _: mcp_call("chat", tool, json.loads(arguments)), range(2))
            reply = " | ".join(response["content"][0]["text"] for response in responses)
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.split("\n\n")[-1] == "OK":
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "(pass)"}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if text.split("\n\n")[-1] == "THANKS":
        response = mcp_call("chat", "react", {"emoji": "❤️"})
        reply = "Held back a reaction" if response.get("isError") else "Reacted with a heart"
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if "PERMISSION" in text:
        send({"id": "permission", "method": "session/request_permission", "params": {
            "sessionId": "session", "toolCall": {"toolCallId": "edit", "title": "Edit a file"},
            "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}],
        }})
        while not cancel.is_set() and not permission.wait(0.01):
            pass
    while "BLOCK" in text and not pathlib.Path("release").exists() and not cancel.wait(0.01):
        pass
    if text.startswith("[Group chat") or "\n[Group chat" in text:
        while "BLOCK" in text and not pathlib.Path("release").exists() and not cancel.wait(0.01):
            pass
        if cancel.is_set():
            send({"id": request["id"], "result": {"stopReason": "cancelled"}})
            return
        # A room turn: answer only when the user spoke last, otherwise pass.
        room = text.split("New messages in the room (oldest first):\n", 1)[-1].split("\n\n", 1)[0]
        last = room.strip().splitlines()[-1] if "New messages" in text else ""
        reply = "reply: group" if last.startswith("User:") else "(pass)"
        if last.startswith("User:") and "PING" in last:
            reply = "@b over to you"
        if "REACT" in last:
            response = mcp_call("chat", "react", {"emoji": "👍", "to": "user"})
            assert not response.get("isError"), response
            reply = "(pass)"
        # Two chunks, apart: the streamed entry is rewritten before it's final.
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply[:3]}})
        time.sleep(0.4)
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply[3:]}})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        return
    if cancel.is_set():
        send({"id": request["id"], "result": {"stopReason": "cancelled"}})
    elif "FAIL" in text:
        send({"id": request["id"], "error": {"code": -32000, "message": "fixture failure"}})
    elif "REFUSE" in text:
        send({"id": request["id"], "result": {"stopReason": "refusal"}})
    else:
        if "EMPTY" not in text:
            update({"sessionUpdate": "agent_message_chunk", "content": {
                "type": "text", "text": "reply: " + text.split("\n\n")[-1],
            }})
        send({"id": request["id"], "result": {"stopReason": "end_turn"}})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        send({"id": request["id"], "result": {"agentCapabilities": {
            "loadSession": True, "_meta": {"claudeCode": {}},
        }}})
    elif method in ("session/new", "session/load"):
        servers = request["params"]["mcpServers"]
        with open("sessions.jsonl", "a") as log:
            log.write(json.dumps(request) + "\n")
        pathlib.Path("servers.json").write_text(json.dumps(request["params"]["mcpServers"]))
        send({"id": request["id"], "result": {"sessionId": "session", "models": {
            "currentModelId": "old", "availableModels": [{"modelId": "old", "name": "Old"}, {"modelId": "new", "name": "New"}],
        }}})
    elif method == "session/set_model":
        # 用于执行中更改配置的回归；当前回合仍按旧模型身份完成。
        send({"id": request["id"], "result": {}})
    elif method == "session/prompt":
        cancel = threading.Event()
        permission = threading.Event()
        threading.Thread(target=prompt, args=(request,), daemon=True).start()
    elif method == "session/cancel":
        cancel.set()
    elif request.get("id") == "permission":
        permission_reply = request.get("result")
        permission.set()
    elif "id" in request:
        send({"id": request["id"], "result": {}})
