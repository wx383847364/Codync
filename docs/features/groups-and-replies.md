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
  `status` / `activity` are its busy member's (`"Alice: Writing a reply…"`); its `lastMessage`
  names who spoke (`"Alice: …"`).
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
| `createBot` | `{kind:"group", name, description?, members}` | Returns the existing group when these exact bots already share one. |
| `updateBot` | `{id, name?, members?, pinned?, hidden?, section?}` | A group can't become a bot or the reverse. `section`: a name, or `null` for none. |
| `deleteBot` | `botId` | A group's bots stay. A deleted bot leaves its groups. |
| `reorderBots` | `ids` | The roster as the user dragged it, top first; groups count as rows. See [bot order](computer-order.md#bot-order). |
| `renameSection` | `from, to` | Every row in section `from` moves to `to`, groups too; an existing name merges them, `null` removes the section. See [bot order](computer-order.md#bot-order). |
| `stop` | `botId` | For a group: ends its room turn and stops members working in it. |
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
The host validates before trimming, then shifts ranges with the trim. Outside bound spans,
full names take priority; compact names and first words must be unique. Unknown `@word`
is text, ambiguous names reject the send and keep the draft. Code fences, inline and
indented code, quote lines, email and URLs do not dispatch. `@all` / `@everyone` expand to
current members; quote replies include their author's target. A message without any
assignment expression asks everyone. Stored history is never parsed again after renames.

Both `speak` and tool-created `task` deliveries occupy the room's wait slot, up to ten
minutes. Timeout only releases that slot; it does not stop the execution or change its
state. Late completion uses a conditional update and cannot revive cancelled work.
The source entry's complete text, attachments and quote (or the exact tool body) are the
request; the newest 24 room messages provide supplemental context only.

Each user entry starts a batch: at most 10 `speak` rows in `claimed`, `started` or
`completed`; claim atomically reserves that budget. Three conversation rounds and a
separate eight-hop chain limit remain independent; tools inherit the round and increment
chain depth. Pending `task` rows (`queued`, `claimed`, `started`) count toward global 64
and per-sender 16 capacity, with checks and insertion in one IMMEDIATE transaction.

The state machine is `queued → claimed → started → completed / noReply / failed /
interrupted / cancelled`. `started` must commit before prompt transmission. Restart
requeues `claimed`, preserves `queued`, and marks `started` interrupted with an uncertain
result; it never automatically replays uncertain work. Startup, idle transitions,
post-commit notification and a roughly three-second scan wake room scheduling.

Final reply, conditional terminal state and derived mentions commit atomically. Cards
aggregate a source event and attempt, with one row per target; retries create a new
delivery and attempt linked to the original. A `speak` pass is silent; a `task` pass says
it provided no result. Group work uses the group lane and never records task text as
user memory or sends its result into the sender's private chat. File/artifact sharing
from a delivery is unavailable; use the final reply, and user attachments remain usable.

Stop, member removal and group deletion persist cancellation before stopping execution.
Private Stop keeps requests queued in other groups. `/new` defaults to switching after
the current execution and result save; `/new-now` cancels, waits for the existing
15-second grace/agent termination, persists interruption, then forgets the old session.
`/new-cancel` withdraws a pending switch. Main and group turns share that session.

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

Slack-style reply threads were removed for quote replies; [threads for teams](../plans/threads-for-teams.md)
lists where their code lived and how to bring it back.
