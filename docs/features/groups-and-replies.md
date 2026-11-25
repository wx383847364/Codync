# Group chats and replies

Both features live in the host. Every client (iPhone, Android, desktop, terminal) calls the same
methods and renders the same data: none of them routes messages, parses mentions or builds
quotes itself.

## Words

- **Chat**: a bot's (or group's) one endless conversation.
- **Reply**: a user message that answers an earlier message of the same chat (the user's own
  or a bot's final reply). It stays in the chat, in time order, and carries a quote of the
  message it answers.
- **Group**: a roster row (`kind: "group"`) holding several bots and the user. It has a
  transcript but no agent, folder or harness of its own.
- **Lane**: where a turn talks: a bot's own chat or a group's. `host/src/store/model.rs` `Lane`.

## Data (all over the existing sync: `bot` and `entry` events)

- Bot: `kind` (`agent` | `group`), `members` (group: one or more distinct agent bot IDs on the
  same computer; no nested groups), `workingChat` (where the running turn talks). A group's
  `status` / `activity` are its busy member's (`"Alice: Writing a reply…"`). A member who
  needs approval takes priority. Its `lastMessage` names who spoke (`"Alice: …"`).
- Entry: `data.author` (the bot that wrote it; every agent-made entry has one); on a reply,
  `data.replyTo = {id, author, text}`: the answered message, who wrote it (`"user"` or a bot
  id) and its text (or its files' names when it has none), at most 500 bytes. The host builds
  it from `send`'s `replyTo` (`host/src/chat/replies.rs`).
- Read state: one per chat (`read_rev`). A bot's `unread` badge counts final replies and pending
  cards newer than it; a reacted-to message doesn't count again.
- The `events` catch-up carries each chat's recent entries; `history` pages older ones.

## Methods (`POST /api/<method>`, same on the E2E channel)

| Method | Body | Notes |
|---|---|---|
| `send` | `botId, text, clientNonce, replyTo?, mentions?: [{botId,start,end}]` | `botId` is a bot or a group. `replyTo`: the id of a user message or final reply in that chat; anything else is refused. Works in the relay mailbox too. |
| `createBot` | `{kind:"group", name, description?, members}` | Returns the existing ordinary group when these exact bots already share one. |
| `createGroupSession` | `{botId, requestId, name, leaderId, roles}` | Copies the reviewed role configuration to new bot IDs and creates an empty, independent group with a stable creation receipt. Legacy requests without `roles` reuse the existing bots. |
| `createStageGroup` | `{botId, requestId, name, goal, acceptance, leaderId, referencePrevious, handoffVersion, roles: [{botId, sourceRev, description, reuseProject}]}` | Creates a stage and its new bots in one transaction. `requestId` is a UUID. Identical retries return the live group, its first draft and `createdMembers`, the original ordered IDs. `sourceRev` binds the preview to the reviewed source settings. The lead is selected by its source bot ID and saved as its new bot ID. |
| `stageDetails` | `{botId}` | Reads stage, delivery counts and pending items, handoff content, and the restorable first draft. |
| `changeStage` | `{botId, action, markdown?, evidence?, previousVersion?, version?}` | `saveHandoff` writes an immutable Markdown snapshot. `accept` binds acceptance to its file ID. `close` and `reopen` control admissions. |
| `groupWorkspaceUsage` | `{roomId}` | Returns each current member’s Codync-managed workspace size, a combined total, and shared-member markers. Directory paths are returned only for local calls. |
| `stageUsage` | `{botId}` | Reads local delivery, attempt, observed ACP prompt, confirmed new session and process counts, provider-reported totals by backend/model, and coverage. |
| `updateBot` | `{id, name?, members?, pinned?, hidden?, section?}` | A group can't become a bot or the reverse. `section`: a name, or `null` for none. |
| `deleteBot` | `botId` | A group's bots stay. A deleted bot leaves its groups. |
| `purgeGroup` | `botId` | Requires the `purgeGroup` capability in `hello`. Removes a group, its exclusive bots, and their Codync data. Copied groups and bots and members shared with other live groups are preserved. |
| `reorderBots` | `ids` | The roster as the user dragged it, top first; groups count as rows. See [bot order](computer-order.md#bot-order). |
| `renameSection` | `from, to` | Every row in section `from` moves to `to`, groups too; an existing name merges them, `null` removes the section. See [bot order](computer-order.md#bot-order). |
| `guideDelivery` | `deliveryId, activeDeliveryId` | Joins a directly mentioned member's live group turn. The host checks the displayed active delivery ID, room, membership and adapter support. |
| `stop` | `botId` | For a group: ends its room turn and stops members working in it. |
| `setDeliveryPaused` | `botId, deliveryId, startedAt, paused` | Interrupts one running group delivery or queues that same delivery to continue. The group snapshot's `deliveryControls` supplies the task ID and current turn's start time. Stale requests leave the delivery as it is. |
| `react` | `entryId, emoji` | Toggles the user's reaction on a chat message (`data.reactions`, oldest first, at most 8). A reacted-to message never turns unread again. The bot isn't told, except in a group, where members see reactions on the room lines they're shown. Bots react themselves with the `chat` server's `react` ([bot reactions](chat-messages.md#bot-reactions)). |
| `respondPermission` | `entryId, optionId?` | Routed to `data.author`, so cards in a group reach the bot that asked. |
| `markRead` | `botId` | Reads the chat. A no-op when nothing there is unread, so clients call it whenever a chat is on screen. `send` marks its chat read. |

## How a group answers (`host/src/chat/group.rs`)

A persistent `deliveries` row is the sole dispatch authority. Each room has one runner,
ordered by monotonic `room_seq`; actors conditionally claim only its specified ID. A busy
member stays `queued`, with the same identity, source and sequence. New user messages do
not cancel accepted deliveries. User entries and their `speak` deliveries commit together.

`send.mentions` binds selected members by ID using UTF-16 half-open ranges into the actual
request body. Clients preserve these bindings in drafts, offline sends, retries and history.
The member menu cycles with up/down in both directions, including first/last wraparound
and repeated key presses. Enter/Tab inserts without sending; Esc closes the menu.
The host validates before trimming, then shifts ranges with the trim. Outside bound spans,
full names take priority; compact names and first words must be unique. Unknown `@word`
is text, ambiguous names reject the send and keep the draft. Code fences, inline and
indented code, quote lines, email and URLs do not dispatch. `@all` / `@everyone` expand to
current members; quote replies include their author's target. A message without any
assignment expression asks everyone in ordinary and numbered session groups. In a stage
group it asks the configured lead. If the lead has left, choose a recipient explicitly.
Stored history is never parsed again after renames.

Both `speak` and tool-created `task` deliveries share one wait slot per original user
message, up to ten minutes. While a member works on an earlier message in the same group,
the runner can deliver a newer message to its idle recipients in order. The busy member's
delivery stays queued under its original source. Timeout only releases that batch's slot;
the execution continues and its state is kept. Late completion uses a conditional update
and cannot revive cancelled work.

The original user entry records direct targets in `data.directMentions`. The card's
`deliveryRows[].canGuide` marks a queued original direct mention. A working member with
ACP steering publishes `guidingDeliveryId` in its bot snapshot. The client sends that
displayed ID with `guideDelivery` for the selected row. The host reserves the queued row as
`started` before transmitting the guidance and saves its outcome with the active delivery.
An adapter that returns `promptRequired` leaves the row queued at its original position.
An uncertain receipt leaves it interrupted and eligible for manual retry. A confirmed
injection joins the same reply and is never replayed by the scheduler.
Every turn protects the complete original user task, current source (or exact tool body),
attachments, structured mention IDs and UTF-16 ranges, direct quotes, later user messages,
and saved open plans. Entries are deduplicated by ID, never by matching text. Later user
messages retain their own task and reply identities; they are not silently rebound as
new authorization for an earlier task. Plans are tracked by entry, so another assignment's
new plan cannot hide earlier open items. The delivery ledger remains authoritative for
pending, failed and interrupted work; normal speak passes and superseded failures are not
open work. Displaying at most 64 ledger headers is pagination, not a work limit.

Supplemental room progress uses sourced excerpts with a 2,048 estimated-token and 8,192-byte
budget. This replaces the fixed 24-message window; only actual shown IDs are reaction
candidates. Protected material is exempt and never shortened to fit. `search_history`
inside a live delivery reads only that source group's visible messages, plans and delivery
bodies, with bounded discovery and exact Unicode-scalar pages. Each page revalidates the
execution and membership; continuation requires its content `version`. An updated plan
requires restarting from offset zero, rather than joining different versions. Large
attachment/quote metadata has its own lossless `part=metadata` pages.

Existing sessions still accumulate agent/tool context. When reported usage reaches the
smaller of 64k and 75% of the model window, or the incoming task needs more space, known
adapters get native compaction in the same session before the next delivery is claimed,
after previous results are durable. Maintenance has no business tools, memory attribution,
ordinary final reply or derived dispatch. Its potentially private summary stays in a
private checkpoint; the group sees only a content-free status. Failed, unconfirmed or
ineffective attempts do not retry endlessly on unchanged context. Unsupported adapters
continue with bounded background. Known insufficient space leaves the exact delivery
queued with a visible notice. If the task and handoff can fit in a fresh session, the
original dispatch gets a review action. An isolated group reviews its group-only summary
there. Ordinary groups link to the member's private chat and explain that both chats
share the session. The approved private summary goes into the next private user turn;
the resumed dispatch carries group context. Confirmation archives the old context
reference and resumes the original dispatch. There is no silent session reset.

The member list reads the current execution slot's reported token count on the Host.
The desktop shows it in the conversation's details sidebar, phones show it in group
settings, and the terminal shows it in the members panel. A room saves its display
basis independently of the agent's actual window. At 70% of the selected display basis, or when
a dispatch has a capacity handoff, the member row and latest message header open the same review. An isolated
group can edit and confirm it in the desktop sidebar or the mobile member page.
A shared session opens its review in the member's private chat.
Confirmation of a proactive handoff is persisted before the actor returns to its queue.
It applies only to the same member session after the current turn and result save, and the
next eligible task carries the reviewed summary.

Unknown/stale usage is not zero, and estimates are not provider
billing: full originals, tools, model output and private/shared session history can still
consume substantial tokens. A maintenance timeout cancels maintenance only, not work.

Each user entry starts a batch. Round and chain depth are retained for tracing, without
fixed round, reply-count or handoff-depth cutoffs. Members continue authorized work and
explicitly assign the next action until they deliver the requested result or need the
user's decision, information or approval. They then report the result or question and
wait, without acknowledgement loops. An empty queue is idle, not proof the whole task
was completed. Busy members and timed-out executions keep their accepted assignments.
Pending `task` rows (`queued`, `claimed`, `started`, `paused`) still count toward global 64 and
per-sender 16 capacity, with checks and insertion in one IMMEDIATE transaction. Completed
work releases capacity; it does not consume a lifetime reply budget.

The state machine is `queued → claimed → started → completed / noReply / failed /
interrupted / cancelled`. A user can move a `started` delivery to `paused` and then move
the same delivery back to `queued`. A paused delivery is pending for stage actions and
stays out of scheduling. `started` must commit before prompt transmission. Restart
requeues `claimed`, preserves `queued` and `paused`, and marks `started` interrupted with
an uncertain result. Startup, idle transitions,
post-commit notification and a roughly three-second scan wake room scheduling.

Final reply, conditional terminal state and derived mentions commit atomically. Cards
aggregate a source event and attempt, with one row per target; retries create a new
delivery and attempt linked to the original. Each card starts as one Chinese status line;
expand it for target status, delivery attempt, guidance and retry actions. Neither the collapsed
nor expanded card repeats the source message. Historical all-cancelled relay limits still show their original reason;
mixed outcomes keep counts so active work is not mistaken for stopped work. The terminal
uses its existing selected-message `o` key to expand or collapse these details.
A `speak` pass publishes no reply. A `task` pass says it provided no result. If a member
gives a result and then passes after thinking, the host keeps the result unless tool work
continued or the member requested approval. Group work uses the group lane and never records
task text as user memory or sends its result into the sender's private chat. A delivery can
share files in the current group. Artifacts stay in the bot's private chat.

Stop, member removal and group deletion persist cancellation before stopping execution.
Private Stop keeps requests queued in other groups. `/new` defaults to switching after
the current execution and result save; `/new-now` cancels, waits for the existing
15-second grace/agent termination, persists interruption, then forgets the old session.
`/new-cancel` withdraws a pending switch. Main and ordinary group turns share that session.

Clients with `hello.taskGroupContexts` submit a reviewed role draft for **Create new group session**. The Host creates new bot IDs and an independent, empty chat. Blank names use the source numbering sequence. The agent session belongs to the room, member and backend. Legacy callers use the existing bots and their workspaces and long-term memory.

Clients show **Create next stage group** when `hello.stageGroups` is true. The stage name,
goal, acceptance criteria, confirmed role instructions and chosen lead are captured before
opening the empty chat with its first draft. Sending that draft is a separate user action.
Each role has a new bot ID and a new managed workspace by default. An existing project
directory can be selected explicitly. Role configuration, public skills and currently
authorized connectors can carry over. New roles begin with Codync shared memory and
supported agent memory switches off. The agent's own global memory still applies on
backends that manage it independently. A role with a custom launch command requires a
standard backend configuration before it can be used here. Each role's stage session
is scoped to the stage room, separate from any private chat opened for that role.

In a stage, `message_bot` accepts `returnResult: true` when the sender needs to continue
with the result. A successful final result schedules the sender once, including when the
result mentions it. Failed, cancelled and passed tasks remain visible on their cards.

The **Stage details** sheet shows pending deliveries and recorded outcomes. A user saves
the handoff as Markdown and records a durable evidence reference before accepting that
version. **Close stage** requires an empty active queue and works independently of
acceptance. The check and stage state update share a write transaction with delivery
admission. A closed stage keeps its history, handoff and late records. Reopening lets the
user send or explicitly retry without changing previous terminal states.

With `referencePrevious: true`, stage creation binds the direct source group and its chat revision at commit. New roles may query visible source messages through `search_history` with `scope: previousStage` during a live stage assignment. The same tool reads an accepted handoff by version when one was present and reviewed. Prompt assembly exposes only the availability of the reference. Users can select files already shared to the source room
as evidence. The Host checks their versions and copies the selected snapshots to the
new room so they can be opened after the old files or chat are removed. The new roles
start without the previous transcript. The
**Restore first draft** action is available before a stage has any messages.

### Registered local tasks

Ordinary groups and stages share the local task tools on the Host's `chat` MCP bridge.
`list_local_tasks` lists the IDs and titles in `local-tasks.json` in the Host data folder.
`start_local_task {taskId, contextDeliveryId}` admits one registered task per active
group assignment. The existing bot approval setting applies. The actor can finish its
turn immediately; the original dispatch stays active until the Host saves the result.
The card's **View result** action opens the summary. Evidence is downloadable from the result or its group attachments. Retransmitting within the same execution reuses the
reservation. A stale execution cannot start work or publish files.

The local registry maps IDs to `{title, program, args?, timeoutSeconds}`. Use a trusted
absolute executable and fixed argument array, with a timeout of 1–3,600 seconds.
The executable runs in a fresh work directory. It writes
`{status, summary, evidence?: [relativePath, …]}` to the file named by
`CODYNC_TASK_RESULT`. The statuses are `passed`, `failed`, `blocked`, and
`needs_review`. Standard output and error stay in local log files. Evidence must
resolve within that run folder and uses the same immutable snapshots as shared files.
A successful exit is required for a passed or review result. A result that still
needs review can be opened without waking a stage's originating bot.

Jobs run one at a time on the Host. Pause or Stop ends the owned run; a later
Continue starts a fresh execution when the bot requests it again. On Host restart,
an assignment that was executing is marked interrupted using delivery recovery.
Only explicit stage return routing can wake a requesting bot. Report text cannot
trigger mentions or further work.

**Stage usage** reads local execution observations. A delivery can have several
attempts after retry or fallback. An attempt that fails during setup can have a
process start and zero observed prompts. One ACP prompt may result in several
provider calls. The Host records the actual session model when it is known and
keeps attempts interrupted by a restart. The count of started deliveries without
an attempt and the recording start date expose gaps in historical coverage.

Token fields come from the final response of each observed attempt and are grouped
by backend and model. The reports count beside a field is how many of those attempts
reported it. Unavailable fields, observed zero values and interrupted attempts are
distinct. Input, cache and reasoning fields may overlap, so they are shown as received.
Session-context estimates and account-window percentages remain separate. Unassigned
counts cover the whole Host since observation began, including private chats, routines,
and coordination. Bot actor attempts are observed. Background one-shot jobs and account
probes are outside this ledger. The SQLite
`turn_usage_attempts` table stores local attempt details without prompts or tool content.

On result storage failure, the actor retains its in-memory output and stops claiming.
It retries every three seconds five times, then waits for `retryDeliverySave {botId}`.
`deliveryStorageError` bypasses DB writes and is included (including null clear) in every
reconnection snapshot. A continued write failure followed by process loss can lose the
unsaved result; recovery says the outcome is uncertain rather than claiming completion.

## How a reply reaches the agent (`host/src/chat/prompt.rs` `of_entry`)

In a bot's own chat a reply is an ordinary message in the same session. A note before its text
says which message it answers: `[Codync: this replies to your message: "…"]`, `… to the user's
earlier message: "…"]` or `… to Ann's message: "…"]`. It speaks as Codync, so the agent trusts the
quote even when the quoted message is no longer in its session. Memory never takes the
quote as the user's words: it stays in `data.replyTo`, apart from `data.text`.

## Client checklist

At the end of the group transcript, clients show one activity label per busy member whose
`workingChat` matches the group, in roster order. The group projection can show the scheduling
status between deliveries.

Roster row for groups (member avatars), a trace per turn, author label on group replies,
**Reply** on user messages and final replies (desktop: the hover footer beside the time, with
quick reactions and copy, never on the bubble, and the context menu; iPhone and Android:
long-press, reactions on top of the menu; terminal: `r` picks a message, ↵ or `r` replies, esc
cancels). While replying, the composer shows a quote
bar on top (`text.bubble`, “text” on one line, an X to cancel; the terminal: `↳ “…”`), its
placeholder reads "Reply…", and sending includes `replyTo`; leaving the chat cancels it. A
user message with `data.replyTo` shows the quote at the top of its bubble. Every client shows a
quote as its first non-empty line without Markdown markers (the roster's message preview rule). Reactions under
a message (the user's: tap to take back; the bots': emoji plus the avatars of who chose it),
group create/edit (name, members), Stop in a group calls `stop` with the group id.
The group roster's **Create new group session** action calls `createGroupSession` and
opens its returned chat, through right-click on desktop, touch-and-hold on phones and
the action search in the terminal.

Slack-style reply threads were removed for quote replies; [threads for teams](../plans/threads-for-teams.md)
lists where their code lived and how to bring it back.
