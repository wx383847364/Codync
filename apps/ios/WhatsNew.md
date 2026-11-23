<!--
App Store "What's New" for the iPhone app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per App Store language (zh-Hant, en-US),
listing what iPhone users notice, without emoji (the App Store rejects them; asc-submit.py drops
any left). tools/asc-submit.py reads the released version's section;
a version without one gets a generic "Bug fixes and improvements" line. Delete old sections.
-->

## 2.50.0

### zh-Hant
- 群聊訊息的成員用量旁顯示會話確認的模型和推理等級。
- 在群組詳情查看檔案占用、累計 Token 及其在主機總用量中的占比。
- 可用新機器人開啟獨立群組任務，或建立下一階段並按需查閱上一階段的聊天與交接。
- 可暫停或繼續群組中的單一任務，並將新訊息引導至成員正在執行的回合。
- 在輸入框鍵入 / 搜尋工作區檔案，選取後以 @檔名帶入訊息。
- 在機器人詳情查看主會話的上下文與近期 token 用量。私聊任務預計放不下時，可以帶著摘要開啟新會話，保留同一個機器人及聊天記錄。
- 點按群組標題，在成員清單查看上下文用量，或審閱摘要以開啟新會話。等待中的原派工會自動接續。
- 群組的預設收件者提示只在輸入框空白時顯示。

### en-US
- See the confirmed model and reasoning level next to a group member’s context usage.
- See group storage, total tokens, and the group’s share of Host usage in group details.
- Start an independent group task with fresh bots, or let a new stage consult the previous chat and handoff on demand.
- Pause or continue individual group assignments and guide a member during its current turn.
- Search workspace files with / and insert a file reference into your message.
- See the main session's context and recent token usage in bot details. When a private task may exceed the available context, start a fresh session with a summary and keep the same bot and chat history.
- Tap the group title to see context usage in the member list or review a handoff. Waiting assignments continue in the new session.
- The group recipient hint appears only when the message box is empty.

## 2.45.2

### zh-Hant
- 展開階段群組即可查看該階段的機器人。原機器人保留在專案清單，舊階段可以封存及恢復。

### en-US
- Expand stage groups to see their bots. Original bots stay in the project list, and you can archive or restore stages.

## 2.45.0

### zh-Hant
- 長按 Bot 分享的檔案，即可開啟、另存或複製分享當時的檔案內容。
- 搜尋並加入群組成員，選擇預設負責人。從原群組開啟空白會話時可調整成員。
- 選擇至少一位成員即可建立下一階段任務群組，名稱、負責人、目標、驗收標準與交接皆可選填。階段詳情提供交接驗收、關閉與用量。

### en-US
- Touch and hold a shared file to open it, save a copy, or copy its text.
- Search for group members, choose a default lead, and adjust the roster when starting a fresh group session.
- Create a next-stage group by selecting at least one member. The name, lead, goal, acceptance criteria, and handoff are optional. Accept handoffs, close stages, and view stage usage.

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
