<!--
App Store "What's New" for the iPhone app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per App Store language (zh-Hant, en-US),
listing what iPhone users notice, without emoji (the App Store rejects them; asc-submit.py drops
any left). tools/asc-submit.py reads the released version's section;
a version without one gets a generic "Bug fixes and improvements" line. Delete old sections.
-->

## 2.45.0

### zh-Hant
- 長按 Bot 分享的檔案，即可開啟、另存或複製分享當時的檔案內容。
- 長按群組可用原有成員建立新的空白會話，成員從各自的新會話開始。

### en-US
- Touch and hold a shared file to open it, save a copy, or copy its text.
- Touch and hold a group to start a fresh conversation with the same bots. Each bot gets its own session in the new group.

## 2.43.0

### zh-Hant
- 群組可同時顯示多位 Bot 的工作狀態和用時。
- 點一下對話中的檔案連結或行內程式碼中的絕對路徑，即可複製電腦上的路徑。
- 圖片可切換原始像素、縮放和平移，並限制快取與讀取資源。
- Bot 可在目前群組明確分享檔案；文字路徑不會自動分享。
- 群組協作持續至交付或需要你的回覆，不再受固定次數限制。
- 群組減少重複背景，保留任務原文、要求與待辦；支援整理上下文，空間不足時保留排隊並提示處理。
- @ 成員支援方向鍵循環選擇；草稿、離線及重送保留身分。
- 可傳大型附件與預覽圖片；派工詳情預設收合，不重複全文。
- 不登入也能用 Wi-Fi 或 Tailscale；帳號可啟用中繼。重試保留附件，儲存故障可重試保存。

### en-US
- See each active group member’s status and elapsed time.
- Tap file links or absolute paths in inline code to copy the computer’s path.
- Inspect original image pixels with zoom and pan, with bounded caching.
- Bots can explicitly share files in their current group.
- Groups continue until delivery or your input, without fixed turn limits.
- Less repeated group context; original tasks, requirements and open work stay intact. Supported agents compact context; insufficient space keeps work queued with guidance.
- Cycle @ members with arrow keys; drafts and retries keep identities.
- Large files, image previews and folded delivery details.
- Wi-Fi/Tailscale without an account; relay with one. Attachments and unsaved results survive retries.
