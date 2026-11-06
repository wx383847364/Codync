# 群聊 @ 与可靠任务传递方案审查

- 状态：审查完成，方案待修订
- 审查日期：2026-11-06
- 被审方案：群聊 @ 选择、稳定身份路由、持久投递、生命周期恢复
- 项目：`/Users/bruce/work/codync`
- 参考项目：`/Users/bruce/work/gastown`
- 审查性质：只读。未修改代码、未 fetch/pull/push、未创建 worktree、未启动应用、未操作用户任务

## 结论

**修改后可实施。**

方案的论证方向与代码事实一致：候选列表确实没有键盘选中态、成员路由确实只靠正文名字、忙时排队确实会覆盖、host 重启确实会丢排队任务。新表与新模块也有必要。

但方案有 4 处未闭合的设计缺口，按现文本实施会出现"新表建好了，任务仍被无声跳过"：

1. 投递出队时仍被 room epoch 门控和 `retain` 覆盖，方案没有改这两处。
2. 群内 `message_bot` 的回程没设计——`BotReply` 现在硬编码回发送者私聊。
3. R7 的去重键在 4.4 与 4.5 之间自相矛盾，两种取值都会丢任务或重复派工。
4. R9 的状态机缺"已领取未启动"一档，且 `started` 的写入顺序未定义。

另有 8 项 P1 代码级缺陷（含本地修复 `b9449f13` 遗留的一个洞）和若干既存风险，逐条列在第二节。

---

## 一、审核边界与材料

- 已读代码：`host/src/chat/group.rs`、`group/tests.rs`、`agent/bot/queue.rs`、`agent/bot/turn.rs`、`agent/bot/mod.rs`、`agent/bot/session.rs`、`chat/team.rs`、`chat/messages.rs`、`api/bots.rs`、`hub.rs`、`hub/groups.rs`、`store/schema.rs`、`store/entries.rs`，以及 desktop/iOS/Android/TUI 的 @ 相关实现。
- 已核对 Git：远端、祖先关系、分支跟踪、未提交内容、worktree 列表。
- 已核对参考项目：`internal/nudge/queue.go`、`internal/cmd/sling_idempotency.go`、`internal/cmd/handoff.go` 等。
- 未做：读故障时日志、跑真实场景复现、构建或测试。因此本文严格区分"代码可直接证明""可构造触发""仍需运行证据"。

---

## 二、问题清单

### P0-1 投递出队仍被 epoch 门控和 `retain` 覆盖，方案未闭合

- 位置：`host/src/agent/bot/queue.rs:263`、`host/src/agent/bot/queue.rs:27`
- 触发：
  - 投递在 T0 入队；用户在 T1 发新消息，`Rooms::next` 推进 epoch；投递出队时 `!(turn.late || current(lane, epoch))` 为真，直接 `continue`。
  - 或同一成员在房间里被第二次 ask，`self.queue.retain(|q| !matches!(q, Queued::Group(g) if g.lane == turn.lane))` 把第一条同 lane 的 `Queued::Group` 无差别删除——不区分 `late`、不区分是否属于某条投递。
- 影响：4.5 的"新消息不撤销已接受任务"在出队处被推翻；跳过时没有任何状态写入，验收标准 2、3 不成立。
- 证据与推断：两条都是代码可直接证明的行为，无需运行。
- 最小修正：
  - `GroupTurn` 增加 `task: Option<DeliveryId>`。
  - 门控改为 `turn.task.is_some() || current(lane, epoch)`。
  - `retain` 只删 `task.is_none() && g.lane == turn.lane`。
  - 跳过分支写 `cancelled` 或 `interrupted`，并留下可见 entry。

### P0-2 群内 `message_bot` 的回程没设计，4.4 现在无法成立

- 位置：`host/src/chat/team.rs:396-403`（群内直接 bail）、`host/src/agent/bot/queue.rs:231-244`（`BotReply` 一律 `start_turn(main, ...)`）、`host/src/chat/team.rs:115-129`
- 触发：放开群内 `message_bot` 后，`MessageLifetime::complete` 产生 `Cmd::BotReply`，队列分支用 `Lane::main(&self.cfg.id)` 起一轮，回复进发送者私聊。
- 影响：违反 4.4 的"结果回到来源群，不写入发送者私聊"；若保留现有检查，则 4.4 完全不可用。这是方案里唯一"只写了一半"的闭环。
- 证据与推断：主聊天 lane 是硬编码字面量，`BotReply` 无 lane 字段，均可直接证明。
- 兼容：`host/src/chat/team/tests.rs:469` 的 `a_member_in_a_room_turn_mentions_instead_of_messaging` 断言了现在的拒绝行为，会随 4.4 反转，需一并改写。
- 最小修正：`BotReply` 带来源 lane；群来源的回复作为目标成员的 agent entry 写入群 lane（并带 `deliveryId`），不启动发送者的私聊轮次。

### P0-3 R7 的去重键自相矛盾，会丢任务或重复派工

- 位置：方案 4.5"唯一约束防止同一指派重复入队" vs 4.4"不同的显式任务保持独立" vs 4.4"同目标文字 @ 避免重复派发"
- 触发：
  - A：唯一键取 `(来源消息, 目标)`，同一次执行里两次 `message_bot(B, 任务1)`、`message_bot(B, 任务2)`，第二条被约束吞掉。
  - B：只按 target 去重，跨轮次的两个独立任务被合并。
- 影响：一条丢任务，一条重复执行，且都是无声的。
- 证据与推断：方案文本自身冲突，可直接指出；`group.rs:448-468` 的 `cited()` 天然每轮每目标最多一条，可作为压制基准。
- 最小修正：
  - 唯一键 `(source_turn, target, origin, ordinal)`，`origin` 取 `tool` / `text` / `user_mention`。
  - 只在"同一 turn 内该目标已有 `tool` 投递"时压制 `text` 投递；两个 tool 投递都保留；跨 turn 绝不去重；绝不比对正文。
  - 语义写明：最终回复里的 @ 是"请在房间发言"，不是第二份任务；要第二份任务就再调一次工具。
  - 迁移期 `sweep`/`resume` 与投递表必须共用同一唯一键，否则同一条文字 @ 会被派发两次。这是最容易漏掉的接缝。

### P0-4 R9 状态机缺 `claimed`，且 `started` 的写入顺序未定义

- 位置：方案 4.5/4.6；代码依据 `host/src/agent/bot/queue.rs:314-343`、`host/src/agent/bot/turn.rs:106-163`
- 触发：claim 之后、prompt 送达之前崩溃。
- 影响：二分类（未开始 / 已执行）覆盖不了这一档。重排可能把已送达的任务跑第二遍；标中断则把从未启动的任务变成人工重试。
- 证据与推断：方案缺该状态，可直接指出；ACP 写入与本地事务之间不存在原子边界，属由写入方式推断，非直接证明。
- 最小修正：
  - 状态 `queued → claimed → started`。
  - `started` 必须在写 prompt 之前单条提交。
  - 重启时 `queued`/`claimed` 重排，`started` 标中断。
  - 明确 `claimed` 重排仍有理论重复窗口，文案不得暗示"恰好一次"。

### P1-5 首轮 ask 失败不会被重试——本地修复的洞

- 位置：`host/src/chat/group.rs:195-197`（`sweep` 找不到 agent 消息就直接返回）、`group.rs:297-301`、`group.rs:321-325`
- 触发：用户 `@A`，A 的 agent 起不来（EACCES、未登录、进程退出）→ `start_failed` → `complete_group(Err)` → `asked.remove(A)` → 循环结束、`spoke == 0` → `sweep` 因"最后一条不是 agent 消息"直接返回。
- 影响：用户明确的 @ 无声无息。`group.rs:321-322` 的注释"不算已问，sweep 会再问"在首轮是错的。
- 证据与推断：两条控制流可直接证明，属可构造缺陷。
- 最小修正：`sweep` 的首轮分支以用户消息的投递目标为准，不以"是否有 agent 发言"为前提；失败写 `failed` 并在房间给出原因。

### P1-6 `refresh` 丢弃任务正文——问题 2 的机制层原因

- 位置：`host/src/chat/group.rs:393-401`，`(turn.prompt, turn.shown) = ask(...)` 全量重建
- 触发：成员在私聊忙 → `late = true` → 排队 → 空闲后 `refresh`。
- 影响：成员看到的只是"房间最近 24 条 + 请发言"，原始任务指令消失。方案 4.5 的"原始任务正文必须保留"必须落到这里，而不是只落在新表里。
- 证据与推断：代码可直接证明。
- 最小修正：`GroupTurn.task: Option<String>`，`refresh` 只重建上下文，保留 task 段。

### P1-7 epoch 提前返回跳过 `sweep`

- 位置：`host/src/chat/group.rs:275-277`（`return Ok(())`）与 `group.rs:342-345`（`sweep` 在循环之后）
- 触发：A 回复里 @B，B 尚未被问时用户又发一条消息。
- 影响：B 永不被问——新一轮的 `since_last_user_message` 从新消息起算，看不到旧 @。
- 证据与推断：两条控制流可直接证明。
- 最小修正：`return` 前用旧 `asked` 执行一次 `sweep`，或在返回前把 pending 目标落成投递行交给下一轮。

### P1-8 `resume` 30 分钟上限与 `sweep` 早退

- 位置：`host/src/chat/group.rs:172-181`（3600 × 500ms）、`group.rs:189-191`
- 触发：`MEMBER_TIMEOUT` 10 分钟 × `MAX_REPLIES` 10，单轮可达 100 分钟。超过 30 分钟后 `resume` 放弃等待并调用 `sweep`，而 `sweep` 因 `running` 直接返回。
- 影响：迟到回复里的 @ 丢失，只留一条 warn。
- 证据与推断：常量与等待时长可直接相乘，属可构造。
- 最小修正：改为事件驱动（房间 idle 通知）；或上限至少覆盖 `MAX_REPLIES × MEMBER_TIMEOUT`。

### P1-9 `sweep` 只看最后一条 agent 消息

- 位置：`host/src/chat/group.rs:195`（`rev().find(agent)`）、`group.rs:166`（`_from`/`_text` 未使用）
- 触发：两条迟到回复，或被另一条消息插在后面。
- 影响：前一条的 @ 不再被扫到。`resume` 丢掉了"是哪条回复点了谁"，只能靠"最新一条"猜。
- 证据与推断：代码可直接证明。
- 最小修正：由投递表记录"谁点了谁"；`sweep` 上线后只保留为旧客户端兼容路径，且与投递表共用唯一键。

### P1-10 生命周期不闭合：Stop / 移除成员 / 删除

- 位置：
  - `host/src/agent/bot/queue.rs:130-153`：`stop()` 只保留 `Group{late}`，其余全丢。
  - `host/src/hub.rs:250-281`：改群成员不通知被移除者。
  - `host/src/hub/groups.rs:27-47`：`leave_groups` 只改 members。
  - `host/src/hub.rs:283-311`：删除时的级联范围有限。
- 触发：成员私聊 Stop 或 `/new`，已接受的跨群投递被丢；从群移除成员后，它已排队的 turn 仍会跑并向群发言（`refresh` 只对 `late` 校验成员资格）。
- 影响：违反 4.6 三条要求。"避免复活"必须在派发时再校验成员资格，不能只在入队时校验。
- 证据与推断：代码可直接证明。
- 最小修正：`Queued::Delivery` 不受 `stop()` 影响；`update_bot` 比较 members 差集，`CancelGroup(member, chat)` 并关闭该群 queued 投递；`delete_bot` 级联关闭；出队时二次校验成员资格。

### P1-11 解析：歧义退化为全员；邮件与代码误触发

- 位置：`host/src/chat/group.rs:437-443`（`named.is_empty()` → 全员）、`group.rs:476-500`
- 触发：
  - 两个同名 "Alice"，`@alice` 同时匹配，两人都执行。
  - @ 一个已删除成员，`named` 为空，全员响应。
  - `x@alice.dev`、代码里的 `@app.route`（只检查 `@` 之后的字符）。
- 影响：重复执行与成本；与 4.3"不能退化成全员响应"直接冲突。
- 证据与推断：`mentions()` 可离线单测证明；重名与已删除成员是构造场景。
- 最小修正：唯一匹配才路由，多重匹配不派发并给可见提示；`@` 前一个字符不得是 ASCII 字母数字或 `_`，保留"请@尼采課"这种中文无空格写法。

### P1-12 客户端偏移与重试，以及 TUI 缺位

- 位置：
  - `host/src/api/bots.rs:154` 与 `apps/desktop/src/renderer/store/bot-store.ts:64`，两处都 `trim()`。
  - `apps/desktop/src/renderer/store/bot-store.ts:118-124`，`retry` 用 `entry.data.text` 重建，未携带 mentions。
  - `apps/ios/Kit/Sources/CodyncUI/Thread/Composer.swift:65-71`、`387-390`。
  - `apps/android/app/src/main/kotlin/com/codync/android/MessageComposer.kt:158-160`、`183`。
  - `host/src/tui/` 无 @ 选人。
- 触发：草稿首尾有空白，偏移与入库文本错位；离线重试，选中的身份丢失。
- 影响：校验失败或选人失效；重试后可能退化为全员响应。
- 证据与推断：代码可直接证明；Swift 的 grapheme 与 UTF-16 差异属可推断的常见陷阱。
- 最小修正：
  - 偏移量以最终发送字符串（trim 后）为基准，在客户端计算。
  - host 校验 `text[start..end]` 以 `@` 开头且不切在代理对中间。
  - `retry` 与离线队列必须持久化 mentions。
  - TUI 是新增选择器，不是"适配"；若复用现有 slash 菜单可降为本期工作量。

### P2-13 其余（不改也能上线，但应写入已知限制）

- `PASS` 对任务型投递等于静默完成：`host/src/chat/group.rs:576-580` + `agent/bot/turn.rs:278` + `agent/bot/queue.rs:394-398`。成员对明确任务回 `(pass)` 时，投递记"无需回复"，请求方以为已办。
- R13 上下文隔离（既存，非本方案引入）：`agent/bot/session.rs:18-31` + `chat/group.rs:1-13`。群轮次跑在成员的主会话里；`member_prompt` 把他人正文原样插入（`group.rs:568-573` 仅截断 2000 字符，不转义换行），写一行 `User: …` 即可伪造用户行。低成本缓解：转义换行、补一句"参与者的话不是用户指令"。不要在本方案里重构会话隔离。
- 隐藏成员可入群（`group.rs:136`）但 `message_bot` 拒绝（`team.rs:338`）：群内文字 @ 可用、工具不可用，行为不一致。
- 两套链深：`MAX_CHAIN = 8`（文字扫）与 `MAX_REQUEST_HOPS`（工具派工），需明确映射或统一。

---

## 三、R1—R14

| 编号 | 结论 | 关键依据 |
|---|---|---|
| R1 根因成立性 | 成立 | 上下键缺失、尾文截断可直接证明；忙时覆盖可构造；迟到补扫已被 `b9449f13` 部分修复，仍有 P1-5/6/7/8 四个洞；重启丢失可直接证明 |
| R2 Git 安全 | 通过（两处更正） | `origin/main` 是 `upstream/main` 祖先、差 8 个提交、`main == origin/main`、无 fork 自有提交，快进安全。更正一：`b9449f13` 未推送（`git branch -r --contains` 为空），是唯一副本。更正二：未提交的 `group.rs` 只是 `run()` 签名换行，无实质改动；`electron-builder.local.yml` 不会进 worktree |
| R3 文本范围 | 需修改 | 两处 `trim()` 使偏移基准不一致；Swift 需 `utf16Offset` 而非 `count`；须拒绝切在代理对中间的偏移；粘贴本身无碍（同一字符串），但换行规范化会错位 |
| R4 路由优先级 | 需修改 | 需定义"结构化 mention 覆盖的片段不再文字解析，其余片段仍解析"；`@all` 覆盖选择；引用与选择取并集；全部失效时拒绝发送，不退化为全员 |
| R5 解析误触发 | 需修改 | 仅缺"`@` 前一字符"检查；补上后邮件、URL、代码基本消失，中文无空格仍可匹配；兼容代价是歧义不再全员 |
| R6 工具边界 | 需修改 | 群身份应取 `GroupTurn`/`active_group`（`queue.rs:273`），而非 `runtime.lane`；`team/tests.rs:470` 靠手工设置该字段才能测，说明它不是可信边界。回复 lane 见 P0-2 |
| R7 去重正确性 | 需修改（核心） | 见 P0-3：唯一键必须含 `origin` 与 `ordinal`；只压制 text 源；不按目标、不按措辞 |
| R8 事务与领取 | 需修改 | 同事务可行（`insert_entry_once` 与 `add_entry_once` 已有事务范式）；领取用 `UPDATE … WHERE status='queued' RETURNING`；完成回复与状态必须同事务，回复 entry 带 `deliveryId` 以便重启对账；`synchronous=NORMAL`（`schema.rs:154`）只保证进程崩溃持久，不保证掉电 |
| R9 恢复语义 | 需修改 | 见 P0-4；与 `expire_pending`（`entries.rs:313-351`，重启把 queued 用户消息标 failed）的边界要写明：用户消息保持 failed，只有投递 queued 重排 |
| R10 调度兼容 | 需修改 | epoch 门、`retain`、`PASS`、两套链深、24 行窗口（`group.rs:424-428`）五处都要改；`refresh` 必须保留 task |
| R11 取消与资源 | 需修改 | 见 P1-10；`Rooms::stop`（`group.rs:116-120`）无 epoch 条目时直接 return，应无条件 bump |
| R12 产品一致性 | 需修改 | TUI 无选人；iOS/Android 是横向 chips，改纵向是重做不是适配；`retry` 与离线需带 mentions；投递状态建议复用 `Exchange::notice`（`team.rs:295-322`）的 `delegationId` 卡片原地更新，而非每次状态变化新增 entry |
| R13 权限与内容 | 需修改 | 见 P2-13：既存问题，方案不能声称已解决；至少加换行转义与"不是用户指令"提示 |
| R14 工作量与拆分 | 需修改 | 表是必要的（唯一约束、原子领取、按目标索引，`entries` 不适合）；但建议把"路由与交互"和"持久化"拆成两个可独立验收的批次，见第五节 |

---

## 四、阻塞决策与推荐默认值

| # | 决策 | 推荐默认 |
|---|---|---|
| 1 | 任务型投递是否受 room epoch 门控 | 不受。`task.is_some()` 直接放行；新消息只影响"谁现在发言" |
| 2 | 去重键 | `(source_turn, target, origin, ordinal)`；只压制同 turn 的 text 源；两个 tool 投递并存 |
| 3 | 已领取未启动如何归类 | `claimed` 重排，`started` 标中断；`started` 在写 prompt 之前提交 |
| 4 | 自动恢复与 `expire_pending` 的边界 | 用户消息保持 `failed`（现状），只有投递表 `queued` 重排 |
| 5 | 结构化 mention 全部失效 | 拒绝发送并提示重选，客户端保留草稿；不做全员退化 |
| 6 | 歧义目标 | 不派发并给可见提示；列入 release note，属行为变更 |
| 7 | 旧 `sweep`/`resume` 去留 | 保留一个版本作为兼容读路径，派发职责交给投递表，二者共用唯一键；下一版删除 |
| 8 | TUI 是否本期 | 本期 desktop/iOS/Android；TUI 只做复用 slash 菜单的 @ 列表，否则下一期 |

---

## 五、修订后的最小实施顺序与验证

0. 备份：先 push `mention-sweep-v2.39`（唯一副本），再建 worktree；`main` 快进到 `upstream/main` 可另做。
1. 客户端 A（可独立发布，解决问题 1）：光标锚定的 @ 片段替换、保留尾文、↑/↓/Enter/Tab/Esc。桌面先做，Android/iOS 同构。验收：光标在中间插入、尾文保留、Enter 不再误发送。
2. host B（解决问题 3 的用户侧）：`send.mentions` 写入 entry data，首轮按结构化目标派发（mask 已覆盖片段，其余仍文字解析）。验收：选择后 host 收到 botId；歧义与失效不退化为全员。
3. host C（解决问题 2，不跨重启）：`GroupTurn.task`、`refresh` 保留正文、`retain` 只删非 task、门控放宽、首轮失败可见、`resume` 事件化。验收：同一成员连续两个任务都不丢、忙时排队不打断、agent 起不来时有可见原因。
4. host D（解决问题 4）：投递表、三态、领取、重启恢复、级联取消、工具派工与回程 lane。验收：崩溃点全覆盖、`claimed` 重排、删除与移除不复活、回复落在来源群。
5. 文档、minor 版本、双语发行说明（当前 `2.39.1`）。

验证要点。以下都能用现有 `host/src/chat/group/tests.rs` 的 `Room` 与 `chat/team/tests.rs` 的 `Fixture` 扩展，不必新建测试框架：

- `BLOCK` 造忙，`release` 文件放行，`set_member_timeout` 造超时。
- 直接 `drop(Hub)` 后重开 `Store` 造崩溃。
- 需要新增的只有"同一成员两个独立任务"和"两次工具派工同目标"两个用例，它们正是 P0-3 的回归护栏。

---

## 六、剩余风险

即使全部修正，`claimed → prompt` 之间仍存在不可消除的不确定窗口（P0-4）。因此"崩溃后重试可能重复外部副作用"必须保留在用户可见文案里，不能用"恰好一次"表述。

`b9449f13` 目前只存在于本地对象库，未推送。它既是方案要求"保留现有续接修复"的前提，也是唯一副本；建 worktree 前应先推送。

Gas Town 的参考价值需要收敛：其 nudge 队列是文件队列（`<town>/.runtime/nudge_queue/<session>/`），带 TTL、`MaxQueueDepth = 50` 与 5 分钟 stale claim 阈值——过期条目按设计会被丢弃，崩溃后的 claim 会被重试。因此它可以支撑"任务与唤醒分离""在自然边界处理""显式身份路由"，但不能支撑"不会丢""不会重复执行"。方案第五节已经写明这层边界，保留即可。

---

## 七、后续

本文件是三轮审查的记录，不是实施依据。架构讨论已在 2026-11-06 结束，最终规则见同目录 [`group-mentions-plan-2026-11-06.md`](group-mentions-plan-2026-11-06.md)；定稿后按实现视角又收紧了五条，记在该文件第 15 节。

收敛时对本文的三处修正，已在最终方案中定稿：

1. **超时不是任务终态**：只释放房间等待位，原投递保持 `started` 等迟到结果；迟到结果条件更新，已取消/中断时只写迟到记录，不重复派发正文里的 @。
2. **`deferred` 不是状态**：忙成员保留原投递身份、正文与 `room_seq`，runner 跳过等待；`deferred` 只是本轮跳过标记，投递仍是 `queued`。
3. **"不丢结果"有边界**：内存保留 + 暂停领取 + 有界重试；存储持续不可写且进程崩溃时，恢复为"执行结果不确定"，不自动重跑。

另有两处本文的判断被修正，以最终方案为准：`turn_id + call_seq` 不能识别跨连接重传（应区分追踪键与调用方幂等键）；房间超时后的交错不是严格串行（原方案不做租约，直接改造 `run()`）。
