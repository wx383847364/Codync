<!--
App Store "What's New" for the iPhone app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per App Store language (zh-Hant, en-US),
listing what iPhone users notice, without emoji (the App Store rejects them; asc-submit.py drops
any left). tools/asc-submit.py reads the released version's section;
a version without one gets a generic "Bug fixes and improvements" line. Delete old sections.
-->

## 2.42.0

### zh-Hant
- 群組協作不再被固定輪數、回覆次數或交接深度截斷；交付成果或需要你的回覆時才等待。
- @ 成員支援方向鍵循環選擇，草稿、離線與重送保留成員身分。
- 可傳送大型附件，傳送前可預覽圖片；派工詳情預設收合，不重複訊息全文。
- 不登入也能透過 Wi-Fi 或 Tailscale 連線；加入帳號後可使用中繼連線。遠端畫面遵循連線設定。
- 重試不會遺失附件；新會話可等目前工作完成，或立即停止後切換。儲存故障會保留結果並提供重試。

### en-US
- Groups keep collaborating until delivery or your input is needed.
- Cycle through @ members with arrow keys; drafts and retries keep their identities.
- Send large files and preview images before sending. Delivery details fold without repeating messages.
- Continue without an account over Wi-Fi or Tailscale; add an account for relay access.
- Retries keep attachments. New sessions can wait or stop current work. Storage errors retain results for retry.
