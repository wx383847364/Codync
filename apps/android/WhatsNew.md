<!--
Google Play release notes for the Android app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per Play language (zh-Hant, published as zh-TW;
en-US), listing what Android users notice, each under 500 characters (Play's limit;
tools/play-submit.py cuts longer notes at the last whole line). tools/play-submit.py reads the
released version's section; a version without one gets the PRs' "What's New: Android" bullets,
else a generic "Bug fixes and improvements" line. Delete old sections.
-->

## 2.42.0

### zh-Hant
- 群組協作持續至交付或需要你的回覆，不再受固定次數限制。
- 群組減少重複背景，保留任務原文、要求與待辦；支援整理上下文，空間不足時保留排隊並提示處理。
- @ 成員支援方向鍵循環選擇；草稿、離線及重送保留身分。
- 可傳大型附件與預覽圖片；派工詳情預設收合，不重複全文。
- 不登入也能用 Wi-Fi 或 Tailscale；帳號可啟用中繼。重試保留附件，儲存故障可重試保存。

### en-US
- Groups continue until delivery or your input, without fixed turn limits.
- Less repeated group context; original tasks, requirements and open work stay intact. Supported agents compact context; insufficient space keeps work queued with guidance.
- Cycle @ members with arrow keys; drafts and retries keep identities.
- Large files, image previews and folded delivery details.
- Wi-Fi/Tailscale without an account; relay with one. Attachments and unsaved results survive retries.
