// HOST_PORT, HOST_TOKEN and FILE_AGENT identify the paired worktree sandbox.
const api = (method, body) => {
  const res = http.post(`http://127.0.0.1:${HOST_PORT}/api/${method}`, {
    headers: { Authorization: `Bearer ${HOST_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (res.status !== 200) throw new Error(`${method}: ${res.status}`)
  return json(res.body)
}
if (typeof CLEANUP !== 'undefined') {
  if (output.sharedFileBot) api('deleteBot', { botId: output.sharedFileBot.id })
} else {
  if (typeof FILE_AGENT === 'undefined' || !/^\/[-a-zA-Z0-9_./]+$/.test(FILE_AGENT)) throw new Error('Set FILE_AGENT to the absolute fixture path')
  output.sharedFileBot = api('createBot', {
    name: `Shared report ${Math.random().toString(36).slice(2, 8)}`, backend: 'custom',
    command: `python3 -X utf8 -u ${FILE_AGENT}`, connectors: [], skills: [], notify: false,
  }).bot
  api('send', { botId: output.sharedFileBot.id, text: 'share-generated-report' })
}
