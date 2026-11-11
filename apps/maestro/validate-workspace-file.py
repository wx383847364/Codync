"""在临时 CI runner 上让安装后的手机应用完成真实 Host 配对、搜索和发送。"""

import json
import os
from pathlib import Path
import subprocess
import threading
import time
from urllib.request import Request, urlopen


PORT = 19391
ROOT = Path.cwd()
WORK = Path(os.environ["RUNNER_TEMP"]) / "workspace-file-validation"
HOST_HOME = WORK / "host"
BASE = f"http://127.0.0.1:{PORT}"


def wait_for(check, seconds=20):
    """等待受控测试进程启动或返回结果。"""
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        try:
            value = check()
            if value:
                return value
        except (OSError, ValueError):
            pass
        time.sleep(.2)
    raise RuntimeError("Timed out waiting for the local host.")


def health():
    """检查当前 runner 的 Host 已启动。"""
    with urlopen(f"{BASE}/health", timeout=1) as response:
        return json.load(response)


def api(method, body):
    """用临时 Host 自己的令牌调用本机 API。"""
    token = (HOST_HOME / "token").read_text().strip()
    request = Request(
        f"{BASE}/api/{method}", data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
    )
    with urlopen(request, timeout=4) as response:
        return json.load(response)


def approve_pairing():
    """自动确认这个无外部访问的 runner 上唯一的临时设备。"""
    requests = api("pairingRequests", {})["requests"]
    if not requests:
        return False
    api("decidePairing", {"requestId": requests[0]["requestId"], "approve": True, "join": False})
    return True


def run():
    """发送前保留短引用，发送后要求测试 Agent 读出原文件内容。"""
    wait_for(health)
    agent = ROOT / "host/tests/fixtures/team_agent.py"
    bot = api("createBot", {
        "name": "Workspace Reference Check", "backend": "custom", "cwd": str(WORK / "workspace"),
        "command": f'python3 -X utf8 -u "{agent}"', "connectors": [], "skills": [], "permission": "auto",
        "notify": False,
    })["bot"]["id"]
    token = (HOST_HOME / "token").read_text().strip()
    print(f"::add-mask::{token}", flush=True)
    # Maestro 初始化驱动可能很慢，待 UI 准备好才发短时有效的配对码。
    approver = threading.Thread(target=wait_for, args=(approve_pairing, 540), daemon=True)
    approver.start()
    subprocess.run([
        str(Path.home() / ".maestro/bin/maestro"), "--device", os.environ["CODYNC_TEST_DEVICE"],
        "test", "--test-output-dir", str(WORK / "maestro"),
        "-e", f"HOST_PORT={PORT}", "-e", f"HOST_TOKEN={token}", "apps/maestro/workspace-file-validation.yaml",
    ], check=True, timeout=600)
    history = api("history", {"botId": bot})["entries"]
    sent = [entry["data"] for entry in history if entry["kind"] == "user"]
    assert len(sent) == 1 and sent[0]["text"] == "READ_WORKSPACE_FILE @guide.md"
    assert sent[0]["workspaceFileReferences"][0]["workspaceFile"]["path"] == "guide.md"


if __name__ == "__main__":
    (WORK / "workspace").mkdir(parents=True)
    (WORK / "workspace/guide.md").write_text("Selected mobile workspace contents", encoding="utf-8")
    with (WORK / "host.log").open("w") as log:
        host = subprocess.Popen(
            ["host/target/debug/codync-host", "serve", "--port", str(PORT)],
            env={**os.environ, "CODYNC_HOME": str(HOST_HOME), "CODYNC_EXTERNAL_URL": BASE,
                 "CODYNC_VAULT": "folder", "CODYNC_CLOUD": "off", "CODYNC_AGENT_CHECKS": "off"},
            stdout=log, stderr=subprocess.STDOUT,
        )
        try:
            run()
        finally:
            host.terminate()
            host.wait(timeout=25)
