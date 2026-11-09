"""Deterministic ACP agent for delegation tests; no provider or network access."""

import json
import pathlib
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
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


# actor 单测没有可执行的宿主 CLI；通过真实 loopback API 测工具桥，完整 stdio MCP 留给 team_e2e。
def direct_call(config, server_name, tool, arguments):
    """供 actor 单测使用认证 HTTP 桥，完整集成测试仍经过真实 MCP。"""
    body = {"botId": config["botId"], "name": tool, "arguments": arguments}
    request = urllib.request.Request(
        f"http://127.0.0.1:{config['port']}/api/{server_name}Call", data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": "Bearer " + config["token"]},
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            value = json.load(response)
        return {"content": [{"type": "text", "text": json.dumps(value)}]}
    except urllib.error.HTTPError as error:
        value = json.load(error)
        return {"isError": True, "content": [{"type": "text", "text": value["error"]}]}


# stdio 模式先校验名册；actor 单测显式选择 HTTP 桥，维护时直接验证目标工具拒绝。
def mcp_call(server_name, tool, arguments, check_roster=True):
    """按测试模式调用协作工具；读取回复不需要接收者身份参数。"""
    direct = pathlib.Path("direct-api.json")
    if direct.exists():
        return direct_call(json.loads(direct.read_text()), server_name, tool, arguments)
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
            if check_roster and tool == "message_bot":
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


# 维护走独立脚本，不消耗接力步骤；刻意模拟错误输出、权限和 MCP 调用以验证隔离。
def compact(request):
    config = json.loads(pathlib.Path("group-compact.json").read_text())
    with open("compactions.jsonl", "a") as log:
        log.write(json.dumps(request["params"]) + "\n")
    # 多次真实维护各有独立原生事件，不能被宿主按同一个 compactionId 去重。
    compaction_id = "group-native-compact-" + str(len(pathlib.Path("compactions.jsonl").read_text().splitlines()))
    while config.get("hold") and not pathlib.Path("compact-release").exists() and not cancel.wait(0.01):
        pass
    for call in config.get("tools", []):
        response = mcp_call(call["server"], call["name"], call["arguments"], check_roster=False)
        with open("compact-tools.jsonl", "a") as log:
            log.write(json.dumps({"name": call["name"], "response": response}) + "\n")
    if config.get("permission"):
        send({"id": "permission", "method": "session/request_permission", "params": {
            "sessionId": request["params"]["sessionId"],
            "toolCall": {"toolCallId": "compact-permission", "title": "COMPACT_PRIVATE_PERMISSION"},
            "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}],
        }})
        # 即使已取消维护，也等待宿主的明确拒绝，覆盖取消宽限期的迟到请求。
        assert permission.wait(5), "maintenance permission was not answered"
        pathlib.Path("compact-permission.json").write_text(json.dumps(permission_reply))
    if cancel.is_set() and config.get("holdAfterCancel"):
        pathlib.Path("compact-cancelled").touch()
        while not pathlib.Path("compact-cancel-release").exists():
            time.sleep(0.01)
    update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "COMPACT_PRIVATE_AGENT"}})
    update({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "COMPACT_PRIVATE_THOUGHT"}})
    update({"sessionUpdate": "tool_call", "toolCallId": "compact-tool", "title": "COMPACT_PRIVATE_TOOL", "status": "in_progress"})
    update({"sessionUpdate": "plan", "entries": [{"content": "COMPACT_PRIVATE_PLAN", "status": "pending", "priority": "high"}]})
    if config.get("confirmed", True):
        update({"sessionUpdate": "compaction_update", "compactionId": compaction_id, "status": "completed",
                "summary": [{"type": "text", "text": "COMPACT_PRIVATE_SUMMARY"}]})
    if "usage" in config:
        update({"sessionUpdate": "usage_update", **config["usage"]})
    send({"id": request["id"], "result": {"stopReason": "cancelled" if cancel.is_set() else "end_turn"}})


# 真实 ACP 回合夹具：读取有限派工步骤并通过真实 MCP 服务发出任务。
def prompt(request):
    """执行委派、分页读取与取消测试场景，不访问真实模型或账户。"""
    text = request["params"]["prompt"][0]["text"]
    with open("prompts.jsonl", "a") as log:
        log.write(json.dumps(text) + "\n")
    with open("prompt-requests.jsonl", "a") as log:
        log.write(json.dumps(request["params"]) + "\n")
    # 原生维护必须先分流，不能把 /compact 算成普通群接力或用户回复。
    if text.startswith("/compact") and pathlib.Path("group-compact.json").exists():
        compact(request)
        return
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
    # 可选地在被阻塞期间发送真实 ACP 思考事件，便于观察群聊的并发状态。
    thought = pathlib.Path("block-thought.txt")
    if "BLOCK" in text and thought.exists() and not pathlib.Path("release").exists():
        update({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": thought.read_text()}})
    while "BLOCK" in text and not pathlib.Path("release").exists() and not cancel.wait(0.01):
        pass
    # 新任务包以稳定 delivery 标识区分群回合，仍按完整来源而非最后一段指令取请求。
    if text.startswith("[Group chat") or "\n[Group chat" in text or "Protected task material. Context delivery ID:" in text:
        while "BLOCK" in text and not pathlib.Path("release").exists() and not cancel.wait(0.01):
            pass
        if cancel.is_set():
            send({"id": request["id"], "result": {"stopReason": "cancelled"}})
            return
        # 群内真实MCP接力：配置只决定夹具行为，不替代host的来源上下文。
        scripted = pathlib.Path("group-script.json")
        if scripted.exists():
            script = json.loads(scripted.read_text())
            # 有限步骤用于验证长链最终交付，避免固定互相 @ 的夹具无限接力。
            if "steps" in script:
                cursor = pathlib.Path("group-script-step")
                index = int(cursor.read_text()) if cursor.exists() else 0
                cursor.write_text(str(index + 1))
                script = script["steps"][min(index, len(script["steps"]) - 1)]
            if "plan" in script:
                update({"sessionUpdate": "plan", "entries": script["plan"]})
            # 模拟 FirstMessage adapter 的原生自行整理，下一回合必须重新补发 profile。
            if script.get("compacted"):
                update({"sessionUpdate": "compaction_update", "compactionId": "group-script-compact", "status": "completed"})
            if "usage" in script:
                update({"sessionUpdate": "usage_update", **script["usage"]})
            for task in script.get("tasks", []):
                response = mcp_call("team", "message_bot", task)
                assert not response.get("isError"), response
            reply = script["reply"]
            trailing = script.get("trailingPass")
            if trailing in ("toolUpdate", "duplicateToolUpdate", "duringReply", "duringReplyThenPass"):
                # 真实状态变化会继续工作，重复的完成通知不会。
                status = "completed" if trailing == "duplicateToolUpdate" else "pending"
                update({"sessionUpdate": "tool_call", "toolCallId": "report-tool", "title": "Check result", "status": status})
            if trailing in ("duringReply", "duringReplyThenPass"):
                # 工具完成可以穿插在同一段流文本的两个 chunk 之间。
                midpoint = len(reply) // 2
                update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply[:midpoint]}})
                update({"sessionUpdate": "tool_call_update", "toolCallId": "report-tool", "status": "completed"})
                reply = reply[midpoint:]
            update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}})
            if trailing and trailing != "duringReply":
                # 复现结果后又输出 pass；工具事件用来区分早先的进度播报。
                update({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Checking the report"}})
                if trailing == "tool":
                    update({"sessionUpdate": "tool_call", "toolCallId": "after-reply", "title": "Check result", "status": "completed"})
                if trailing in ("toolUpdate", "duplicateToolUpdate"):
                    update({"sessionUpdate": "tool_call_update", "toolCallId": "report-tool", "status": "completed"})
                for chunk in ["(pa", "ss)"]:
                    update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": chunk}})
            send({"id": request["id"], "result": {"stopReason": "end_turn"}})
            return
        # A room turn: answer only when the user spoke last, otherwise pass.
        room = text.split("New messages in the room (oldest first):\n", 1)[-1].split("\n\n", 1)[0]
        lines = room.strip().splitlines()
        last = lines[-1] if "New messages" in text and lines else ""
        # 完整来源不在窗口时仍是本次请求；不能把窗口当作唯一任务正文。
        if "Request (complete original):\n" in text:
            source = text.split("Request (complete original):\n", 1)[1].split("\n\n", 1)[0]
            if source.startswith("User:"):
                last = source
                # 无点名的群请求已有成员贡献时不重复答；明确 @ 的独立请求仍正常执行。
                progress = text.split("Saved room progress excerpts", 1)[-1]
                root = re.search(r"Original user task \(complete\); entryId=([^,]+)", text)
                if "@" not in source and root and ("root=" + root.group(1) + " ") in progress:
                    last = ""
        reply = "reply: group" if last.startswith("User:") else "(pass)"
        if last.startswith("User:") and "PING" in last:
            reply = "@b over to you"
        if last.startswith("User:") and "LATEAT" in last:
            # A late answer: the room turn that asked is over by the time this lands.
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
        # 仅显式夹具文件切换为 FirstMessage，原有 Claude/native 维护用例保持能力不变。
        capabilities = {"loadSession": True}
        if not pathlib.Path("first-message").exists():
            capabilities["_meta"] = {"claudeCode": {}}
        send({"id": request["id"], "result": {"agentCapabilities": capabilities}})
    elif method in ("session/new", "session/load"):
        # 测试在claimed与started之间强杀host，准备会话时绝不发送任务prompt。
        while pathlib.Path("hold-session").exists():
            time.sleep(0.01)
        servers = request["params"]["mcpServers"]
        with open("sessions.jsonl", "a") as log:
            log.write(json.dumps(request) + "\n")
        # 模拟原会话无法恢复；宿主应新建后跳过维护，不能留下 Maintenance 阻塞原投递。
        if method == "session/load" and pathlib.Path("reject-load").exists():
            send({"id": request["id"], "error": {"code": -32000, "message": "fixture cannot reload session"}})
            continue
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
