// 仅在配对表单已经打开时生成临时配对链接，避免模拟器启动耗尽有效期。
const response = http.post(`http://127.0.0.1:${HOST_PORT}/api/pairing`, {
  headers: { Authorization: `Bearer ${HOST_TOKEN}`, 'Content-Type': 'application/json' },
  body: '{}',
})
if (response.status !== 200) throw new Error(`pairing: ${response.status}`)
output.pairingUrl = json(response.body).pairingUrl
