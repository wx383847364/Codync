// Makes the group `mentions.yaml` opens, on the host the phone is paired with (HOST_PORT and HOST_TOKEN, passed with
// `-e`). `CLEANUP`, when the flow completes, deletes it and its members again.
const api = (method, body) => {
  const res = http.post(`http://127.0.0.1:${HOST_PORT}/api/${method}`, {
    headers: { Authorization: `Bearer ${HOST_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (res.status !== 200) throw new Error(`${method}: ${res.status} ${res.body}`)
  return json(res.body)
}

if (typeof CLEANUP !== 'undefined') {
  for (const id of output.made || []) api('deleteBot', { botId: id })
} else if (typeof START_TASKS !== 'undefined') {
  // 两条独立消息各有一个等待位，真实 Host 可让两位成员同时执行。
  const [group, alpha, beta] = output.made
  for (const [botId, name] of [[alpha, output.alpha], [beta, output.beta]]) {
    const mention = `@${name}`
    api('send', { botId: group, text: `${mention} Review the assignment`, mentions: [{ botId, start: 0, end: mention.length }] })
  }
} else {
  const run = Math.random().toString(36).slice(2, 6)
  const taskTest = typeof PAUSE_TEST !== 'undefined'
  // 暂停用例复用可中断的 ACP 夹具，其他用例只需要成员元数据。
  const member = (name, description, seconds) =>
    api('createBot', { name, description, backend: 'custom',
      command: taskTest ? `python3 -X utf8 -u '${STEER_AGENT}' plain ${seconds}` : 'unused',
      connectors: [], permission: 'auto', notify: false }).bot
  const alpha = member(`Mention Alpha ${run}`, 'Reviews the code', 24)
  const beta = member(`Mention Beta ${run}`, 'Writes the docs', 12)
  const group = api('createBot', { name: `Mention Group ${run}`, kind: 'group', members: [alpha.id, beta.id] }).bot
  output.made = [group.id, alpha.id, beta.id]
  output.group = group.name
  output.alpha = alpha.name
  output.beta = beta.name
}
