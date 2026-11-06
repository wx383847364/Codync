<!--
Google Play release notes for the Android app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per Play language (zh-Hant, published as zh-TW;
en-US), listing what Android users notice, each under 500 characters (Play's limit;
tools/play-submit.py cuts longer notes at the last whole line). tools/play-submit.py reads the
released version's section; a version without one gets the PRs' "What's New: Android" bullets,
else a generic "Bug fixes and improvements" line. Delete old sections.
-->

## 2.41.0
### zh-Hant
- 群聊可從游標處選擇成員，保留後方文字與穩定身分；離線及重送也不會遺失已選成員。
- 群內工作會可靠排隊並顯示進度，成員忙碌或新訊息不會取消已接受工作。中斷的工作可手動重試。
- 新建會話可等待目前工作結束、立即停止並切換，或取消切換。儲存故障時會保留結果並提供重試保存。

### en-US
- Pick group members at the cursor while keeping the rest of your message. Selected identities survive offline sends and retries.
- Group work stays queued while members are busy, with visible delivery status and manual retry after interruptions.
- Start a new session after the current task, stop and switch immediately, or cancel a pending switch. Storage errors retain results and offer a save retry.
