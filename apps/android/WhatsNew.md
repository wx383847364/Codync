<!--
Google Play release notes for the Android app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per Play language (zh-Hant, published as zh-TW;
en-US), listing what Android users notice, each under 500 characters (Play's limit;
tools/play-submit.py cuts longer notes at the last whole line). tools/play-submit.py reads the
released version's section; a version without one gets the PRs' "What's New: Android" bullets,
else a generic "Bug fixes and improvements" line. Delete old sections.
-->

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
- Original image pixels with zoom/pan and bounded caching.
- Explicit group file sharing.
- Groups continue until delivery or your input. Tasks and open work survive context compaction.
- Cycle @ members; drafts and retries keep identities.
- Large attachments and folded delivery details.
- Wi-Fi/Tailscale without sign-in; relay with an account. Retry attachments and unsaved results.
