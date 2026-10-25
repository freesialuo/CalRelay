# 标准事件与存储模型

## 1. 建模原则

事件定义、重复实例、提醒策略、投递记录分别建模。核心以经过校验的结构为真相源；保留受大小限制的原始 ICS/JSON 作为诊断与未来迁移依据，不能在每次提醒时重新解析原文。

[event-v1.schema.json](../schemas/event-v1.schema.json) 定义归一化边界；它是 CalRelay 自定义契约，不宣称完全等同 iCalendar 或 JSCalendar。规范参考 [RFC 5545](https://datatracker.ietf.org/doc/html/rfc5545) 与 [JSCalendar](https://datatracker.ietf.org/doc/html/rfc8984)。schema 预留完整的重复例外、警报和扩展结构；每个能力只有通过对应验收后才能启用。

## 2. 身份、来源与修订

| 字段 | 语义 |
| --- | --- |
| `id` | 服务器生成的稳定内部 ID，不接受外部指定 |
| `source_id` | 来源命名空间，从认证上下文注入 |
| `external_uid` | 上游事件 UID；与 source_id 组成唯一身份 |
| `source_sequence` | 上游 SEQUENCE 等协议修订信息，可缺省 |
| `source_modified_at` | 上游最后修改时间，仅作为来源信息 |
| `revision` | 内部单调递增修订，由有效内容变化产生 |
| `content_hash` | 规范化有效内容摘要，重复同步不生成新修订 |
| `created_at / updated_at / deleted_at` | 本地审计时间，UTC |

不以标题、时间或裸 UID 跨源去重；同一会议出现在两个来源时保持两个事件，可由订阅过滤显式排除。来源重命名不改变 source_id。低 source_sequence 的迟到更新不覆盖新事件；对不可信或无序列来源，使用同步运行 CAS、源快照版本和显式冲突策略，不能仅依赖上游时间戳排序。

schema 输入不携带上述服务器字段；存储采用 `EventRecord { metadata, document: EventV1 }`。导入源的 DTSTAMP、未知参数和属性保存在原文/扩展中，导出 DTSTAMP 使用受控的内部修订时间。

## 3. 时间类型

`Temporal` 使用可判别联合，禁止把所有时间先转换成 UTC 后丢弃语义：

| kind | 字段 | 例子 |
| --- | --- | --- |
| `date` | date | 全天事件 `2026-11-02` |
| `utc` | value | `2026-11-02T17:00:00Z` |
| `zoned` | local、tzid | `2026-11-02T09:00:00` + `America/Vancouver` |
| `floating` | local | 不指定时区的当地时间，执行前必须配置解释时区 |

- 全天事件结束日期排他；不能把一天简单等同于 86400 秒。
- recurrence 的当地时间不变，跨夏令时后 UTC 偏移可以改变。
- 保留 IANA TZID 或来源 VTIMEZONE 标识及定义；未知 TZID 且无可解释 VTIMEZONE 时拒绝规划，不默认 UTC。IANA 与自定义同名冲突时使用来源绑定的定义。
- 遵循日历规范的重复展开和歧义时间规则；不存在的重复实例、歧义时间、日期值与时区绑定必须有黄金样例。不可由平台默认时区决定。
- 时区数据库或 VTIMEZONE 更新需要触发未来实例重规划；过去已接受的推送不回放。
- `end` 与 `duration` 互斥，均缺省时按照支持的日历规范推导；全天默认一天，定时默认同一开始时刻。持续时间保留日历 duration 语义，不能一律压成秒数。
- 结构 schema 只做形状约束；日期有效性、duration 正负、结束晚于开始、类型一致性、UTC 精度策略由领域校验，已实现的规则与支持边界见 [domain-capabilities.md](domain-capabilities.md)。

## 4. 事件正文

| 分组 | 字段 |
| --- | --- |
| 契约 | schema_version、external_uid |
| 展示 | title、description、location、url、categories |
| 状态 | tentative / confirmed / cancelled、public / private / confidential、opaque / transparent |
| 时间 | start、end 或 duration |
| 重复 | RRULE、RDATE（日期时间或 PERIOD）、EXDATE、例外覆盖 |
| 参与者 | organizer、attendees（地址、显示名、角色、参与状态） |
| 提醒 | alarms：动作、相对/绝对触发、重复及动作参数 |
| 协议信息 | source_sequence、source_modified_at、timezones、extensions |

描述是纯文本；将来支持 HTML 时另设字段并执行净化。附件仅保存受控链接/元信息，不自动下载或执行。扩展属性用 `name + parameters + value` 保留重复属性与参数，限制大小和数量。所有富内容、URL 与自定义属性均为不可信输入。

## 5. 重复规则与例外

基础重复集合以母事件、RRULE、RDATE、EXDATE 形成；例外以原始 `RECURRENCE-ID` 定位，不以改期后的 DTSTART 定位。`original_recurrence_key` 由类型、原始日期/时间和时区标识规范化生成；非重复事件使用固定 `single`。

例外结构：`recurrence_id + range + change`。change 为 `cancelled` 或 `override`；覆盖字段不存在表示继承，显式 null 仅用于允许清除的可选字段。override.start 是新开始时间，不改变 recurrence_id。`range=this_and_future` 必须保留，首版不可执行时整条序列标记不支持，不错误当作单次覆盖。

RDATE 的 PERIOD 包含开始和结束或持续时间，必须保留独立时长。EXDATE、RECURRENCE-ID 与母事件的值类型和时区要求由语义校验保证。重复例外可以使某个实例取消、改期或改变标题/持续时间/警报。

无限重复不无限展开。实例表是可重建的规划缓存，母事件与例外是持久化真相源。规则需限制最大展开次数、时间跨度、单次运算预算；超限进入可见的规划错误状态，不静默截断后宣称成功。

源不支持的 VTODO/VJOURNAL 等组件可保留原文并报告，不混成 VEVENT。VEVENT 不支持的语义不得被悄悄忽略后继续发提醒。

## 6. 提醒规则与导入警报

导入 `alarms` 与服务端 `reminder_rules` 分开：前者是日历数据，后者决定是否给某个 Bark 输出器发消息。

服务端规则包含日历选择、提前量、全天时间、时区、免打扰策略、追赶窗口、输出绑定、是否使用导入警报、是否启用变更通知。默认忽略源 EMAIL/AUDIO 警报的执行，只保留数据；初版 Bark 不自动执行源指定的收件人或附件。

免打扰的“顺延/跳过”和是否允许晚于事件开始必须显式设置；初版默认跳过并记录原因，不在清晨一次性堆积发送。

## 7. SQLite 逻辑表

以下为完整目标模型。`calendar-storage` 已通过迁移实现 `sources`、`calendars`、`events`、`event_revisions`、`source_changes`（见 [存储基础](storage-foundation.md)、[来源同步](source-sync.md)）以及 `outputs`、`reminder_rules`、`rule_outputs`、`planning_watermarks`、`occurrences`、`logical_deliveries`、`delivery_jobs`（见 [提醒规划](reminder-planning-test-design.md)），以及 `delivery_attempts`（见 [投递恢复](delivery-recovery.md)）。凭据、API/订阅 token 及来源/输出凭据引用由迁移 `0005` 实现（见 [凭据安全](credential-security.md)）；其余表仍待后续迁移，不代表已实现。

| 表 | 关键字段与约束 |
| --- | --- |
| `sources` | id PK、kind、config_json、credential_id、revision、enabled、committed_checkpoint、last_verified_at |
| `sync_runs` | id PK、source_id FK、base_revision、status、started/finished_at、diagnostic_summary |
| `sync_staging` | run_id FK、page_key、payload_ref、complete；提交后可清理 |
| `calendars` | id PK、source_id FK、external_id、timezone_policy；UNIQUE(source_id, external_id) |
| `events` | id PK、source_id FK、calendar_id FK、external_uid、revision、document_json、content_hash、deleted_at；UNIQUE(source_id, external_uid) |
| `event_revisions` | event_id FK、revision、document_json、change_kind、changed_at；PK(event_id, revision) |
| `source_changes` | source_id FK、source_revision、event_id FK、event_revision、change_kind；唯一源修订内的事件变化 |
| `planning_watermarks` | scope_id PK、source_revision、rule_revision、planned_until、tzdata_version、last_error |
| `occurrences` | id PK、event_id FK、original_key、event_revision、start_utc、end_utc、local_date、resolved_tzid、cancelled；UNIQUE(event_id, original_key) |
| `outputs` | id PK、kind、config_json、credential_id FK、generation、enabled |
| `reminder_rules` | id PK、calendar/filter 配置、policy_json、generation、enabled |
| `rule_outputs` | rule_id FK、output_id FK；PK(rule_id, output_id) |
| `logical_deliveries` | id PK、event_id FK、original_key、rule_id FK、output_id FK、notification_kind、accepted_at；以上语义身份 UNIQUE |
| `delivery_jobs` | id PK、logical_delivery_id FK、generation、event/rule/output_revision、due_at、deadline_at、state、next_attempt_at、lease_*、planning_context（解释时区/tzdata）；UNIQUE(logical_delivery_id, generation) |
| `delivery_attempts` | id PK、job_id FK、attempt_no、lease_owner/token、request_id、started/sent/finished_at、outcome、disposition、retry_after_ms；UNIQUE(job_id, attempt_no) |
| `feed_subscriptions` | id PK、public_id UNIQUE、source_id FK、token_digest UNIQUE、scope_json、revision、expires/revoked_at |
| `credentials` | id PK、kind、ciphertext、nonce、key_version、revision、rotated_at；主密钥不在库中 |
| `api_tokens` | id PK、token_digest UNIQUE、source_id FK、can_upsert/can_delete、expires/revoked_at |
| `admin_sessions` | token_digest PK、identity_sub、csrf_digest、expires_at、revoked_at |
| `idempotency_records` | principal、route、key、request_hash、response_ref、expires_at；UNIQUE(principal, route, key) |
| `audit_log` | id PK、actor、action、entity_ref、revision、request_id、created_at；无密钥和事件正文 |

首版 events 身份包含来源而非 calendar：源内 UID 稳定，事件可移动到另一个日历。支持多个外部日历的接入器必须提供源级无冲突 UID 映射；不能依赖不成立的跨日历 UID 唯一假设。日历与来源归属通过复合外键或仓储事务校验防止错绑。

事件 document_json 是规范化结构；repeat/timezone/exception 的所有信息保存在该结构中，热点查询仅投影到索引列或 occurrences。不得同时维护两个没有明确主从关系的事件正文。

源 revision 与 event revision 分开：一次快照可以修改多个事件；source_changes 是可靠规划日志。消费后按规划水位和备份保留策略清理，不在提交后立即删除。

已实现的来源提交契约见 [来源同步事务](source-sync.md)：每个成功批次推进来源 CAS revision，无事件内容变化时不新增事件修订或变化日志；事件/墓碑、日志及 checkpoint 原子提交。不完整快照和未批准的大规模删除整批拒绝。

提醒 generation 是每个逻辑投递的单调任务版本；event/rule/output 的修订分别记录，不能把三者简单拼成可排序整数。租约写入和结果提交都检查 owner/token。作废中的在途请求记录真实返回结果，但不能复活旧任务或覆盖新 generation。

### 必要索引与约束

- 到期任务索引：`delivery_jobs(state, next_attempt_at, due_at)`；租约恢复索引：`(state, lease_until)`。
- 实例范围查询：`occurrences(start_utc, end_utc)`；按事件重算索引：`(event_id, event_revision)`。
- 增量规划：`source_changes(source_id, source_revision)`；清理索引包含 expires_at/deleted_at。
- 所有外键在每条连接启用；修订与尝试次数 CHECK 非负；状态值受 CHECK 限制；正文须满足 JSON 有效性。
- 逻辑投递与事件/规则/输出默认软删除，保留 accepted 去重历史；不能清理一条成功记录后又把其仍在追赶窗口内的实例补发。
- 任务 `accepted` 与逻辑投递 accepted_at、attempt 结果同事务提交。网络在事务外，因此崩溃仍可能形成不确定外部结果。

## 8. 演进与兼容性

- schema_version 必填；不兼容变更发布新版本。旧数据由可回滚部署方案配套的显式迁移升级，不在读取时偷偷修改。
- JSON Schema 负责结构；领域验证器负责跨字段、重复规则和时区；数据库约束负责关系与并发下的不变量。
- 仅接受声明支持的能力档位；无法执行的警报/范围例外进入诊断，不宣称“完整支持 RFC”。
- 对照样例覆盖：全天、UTC、floating、夏令时切换、月末、闰日、EXDATE、PERIOD RDATE、单次改期/取消、THISANDFUTURE、自定义 VTIMEZONE、Unicode 与长行折叠。
