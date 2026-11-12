"""ACP fixture using the real built-in stdio chat MCP, including resumed sessions."""
import json
import os
import subprocess
import sys
from pathlib import Path

sessions = {}


# 将 fixture 的 ACP 回应写回真实执行器管道。
def send(value):
    print(json.dumps(value), flush=True)


# 通过真实 MCP 分享文件，纯路径文本模式不调用发布工具。
def prompt(message):
    params = message['params']
    session = sessions[params['sessionId']]
    tool = next(server for server in session['mcpServers'] if server['name'] == 'chat')
    with subprocess.Popen([tool['command'], *tool['args']], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=os.environ.copy()) as mcp:
        sequence = 0
        # MCP 请求保持顺序，调用结果用于断言真实权限边界。
        def request(method, arguments):
            nonlocal sequence
            sequence += 1
            mcp.stdin.write(json.dumps({'jsonrpc':'2.0', 'id':sequence, 'method':method, 'params':arguments}) + '\n')
            mcp.stdin.flush()
            return json.loads(mcp.stdout.readline())['result']
        request('initialize', {'protocolVersion':'2025-06-18', 'capabilities':{}, 'clientInfo':{'name':'file-test','version':'1'}})
        tools = request('tools/list', {})['tools']
        assert any(tool['name'] == 'send_file' for tool in tools)
        text = params['prompt'][0]['text']
        if 'files-pass-with-progress' in text:
            # 模拟发文件前的进度与思考，且 ACP 不单独报告工具事件。
            for kind, chunk in [('agent_message_chunk', '@Observer still preparing.'), ('agent_thought_chunk', 'Prepare the file')]:
                send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': params['sessionId'],
                    'update': {'sessionUpdate': kind, 'content': {'type': 'text', 'text': chunk}}}})
        results = []
        if 'path-text-only' in text or 'file-link-menu' in text:
            pass
        elif 'invalid-files' in text:
            for path in ['missing', '.', 'oversized']:
                results.append(request('tools/call', {'name':'send_file', 'arguments':{'path':path}}))
        elif 'share-generated-report' in text:
            path = Path(session['cwd'], 'Retest report.md')
            path.write_text('# Retest complete\n', encoding='utf-8')
            results.append(request('tools/call', {'name':'send_file', 'arguments':{'path':path.name}}))
            path.unlink()
        else:
            paths = [Path(session['cwd'], 'selected-file.txt').read_text()] if 'share-selected-file' in text else (['image.png'] if 'share-image' in text else ['binary.dat', '.hidden', 'empty', 'report.pdf'])
            for path in paths:
                results.append(request('tools/call', {'name':'send_file', 'arguments':{'path':path}}))
        Path(session['cwd'], 'file-results.json').write_text(json.dumps(results))
        mcp.stdin.close()
    if 'file-link-menu' in text:
        # 此用例直接返回文件链接，验证回复只引用原路径。
        reply = Path(session['cwd'], 'file-links.md').read_text(encoding='utf-8')
        send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': params['sessionId'],
            'update': {'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': reply}}}})
    elif 'files-only' not in text:
        send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':params['sessionId'],'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':('(pass)' if 'files-pass' in text else (' ' if 'files-blank' in text else 'The files are ready.'))}}}})
    return {'stopReason':'end_turn'}

for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        result = {'protocolVersion':1,'agentCapabilities':{'loadSession':True}}
    elif method in ['session/new', 'session/load']:
        session_id = message['params'].get('sessionId', 'file-test-' + str(len(sessions)))
        sessions[session_id] = message['params']
        if method == 'session/load': Path(message['params']['cwd'], 'resumed').touch()
        result = {'sessionId':session_id}
    elif method == 'session/prompt':
        result = prompt(message)
    else:
        continue
    send({'jsonrpc':'2.0','id':message['id'],'result':result})
