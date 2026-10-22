# 架构设计

设计基线：2026-10-21。项目名 CalRelay；单管理空间、自托管、单服务实例。本文是预设计，技术依赖版本在实现时锁定并重新核对。

## 1. 目标与范围

- 输入：ICS 文件、远程 ICS 订阅、具备鉴权的远程 JSON API、主动提交标准事件的 API。
- 核心：事件归一化、来源隔离、修订与取消、时区与重复规则、提醒规划。
- 输出：可订阅 ICS，以及 Bark 定时推送；保留其他推送渠道的扩展接口。
- 运维：SQLite 持久化、Docker 镜像、健康检查、故障恢复、备份、管理面鉴权与人机验证。
- 首版不做日历双向写回、会议邀请协议、CalDAV 服务端、微服务或多实例高可用。

“协调接入器”解释为统一的接入协调器加可替换适配器。“多模块耦合”采用接口约束依赖：允许业务协作，避免各模块直接读写彼此的数据表。

## 2. 模块与依赖

以下为计划目录，不创建空实现：

```text
crates/
  calendar-domain/       事件类型、时间语义、重复规则、校验；无网络或数据库依赖
  calendar-application/  接入协调、同步提交、事件查询、提醒规划、投递用例
  calendar-adapters/     ICS / HTTP JSON / Bark / ICS 渲染、鉴权实现
  calendar-storage/      SQLite 仓储、事务、迁移、租约、备份
  calendar-server/       HTTP 路由、任务启动、配置、健康检查、依赖装配
web/                     TypeScript 管理界面
schemas/                 跨语言数据契约
docs/                    架构、决策、运维与参考
```

编译依赖方向：`application → domain`；`adapters → application/domain`；`storage → application/domain`；`server → 全部`。接口由 domain/application 定义，外层实现；domain 不引用 SQLx、HTTP 框架或 Bark。应用用例持有事务抽象，跨表提交不由适配器自行拼接。

管理界面只通过版本化 HTTP API 操作 Rust 服务。默认将构建后的静态资源放入同一个镜像，无需 Node.js 生产进程。额外 Python 数据处理器将来可通过标准事件 API 接入，不嵌入核心解释器。

初选组件：Tokio、Axum、Reqwest、Serde、SQLx、tracing；SQLite。ICS 解析、重复展开与时区库须通过兼容性样例评估后选定，不能仅凭“支持 RRULE”宣称完整日历兼容。[SQLx 参考](references.md#实现文档与公共代码)

## 3. 接入协调器

协调器负责触发、源级并发控制、鉴权注入、分页与游标生命周期、限流、重试、标准化、完整性检查和事务提交。适配器负责协议转换，不直接调度提醒或发送推送。

接口概念：

```text
SourceAdapter.fetch(context, committed_checkpoint)
  -> FetchOutcome::NotModified | SyncBatch

SyncBatch {
  source_id, run_id, mode: FullSnapshot | Delta,
  records: [Upsert(EventV1) | Delete(external_uid)],
  proposed_checkpoint, completeness, diagnostics
}

EventRepository.commit_sync(batch) -> CommittedSourceRevision
ReminderPlanner.reconcile(source_revision, planning_window)
FeedRenderer.render(subscription, calendar_snapshot) -> IcsDocument
PushOutput.deliver(delivery_envelope, credential_handle) -> DeliveryOutcome
```

以上是语义接口，不是可编译 Rust trait。完整快照与增量必须显式声明；分页数据先暂存，收齐所有页面才允许作为完整快照提交。单个源同一时刻只有一个同步运行可提交；使用源版本 CAS 拒绝迟到结果。

### 输入适配器

| 适配器 | 行为 | 鉴权 |
| --- | --- | --- |
| ICS 文件 | 管理 API 上传，限制大小，解析为事件集合 | 管理会话 |
| ICS HTTP | 条件 GET、ETag/Last-Modified、完整快照 | 无、Basic、Bearer、OAuth2 |
| HTTP JSON | 按明示映射生成标准事件；支持分页和增量游标 | Basic、Bearer、OAuth2 |
| 标准事件 API | 按来源 upsert/delete，不信任正文的来源归属 | 限定来源作用域的 API token |

网络传输和事件编码分离：ICS 也可通过鉴权 HTTP 获取。HTTP JSON 映射限声明式字段映射，不允许在服务内执行任意脚本。复杂源用外部进程提交标准事件。

凭据只在调用时由凭据提供器解密；OAuth2 刷新按凭据串行化，新的 refresh token 持久化成功后才结束刷新。401 可刷新一次再重试；invalid_grant 暂停来源并要求重新授权。授权回调校验 state，支持 PKCE，限制 redirect URI；新来源 URL 及重定向遵守 SSRF 规则。[安全设计](operations.md)

### 同步提交规则

1. 网络获取和解析在事务外进行，保留源数据摘要与诊断。
2. 对所有事件进行结构和语义校验；首版任一无效记录使整批不提交，避免把解析失败当作删除。
3. 短事务内核对源版本，更新事件修订、删除墓碑、同步游标和源修订；置规划水位为待推进。
4. 完整快照仅在可信且收齐后按“缺失即删除”处理；空快照遇到大规模删除阈值时暂停并要求管理员审核。增量只认显式删除。
5. 规划器读取持久化修订差异，生成实例和任务；崩溃后从旧规划水位重做。推进水位与对应任务更新同事务提交。
6. 304 仅更新成功核验时间；不改事件修订。首次出现 304 而无本地快照是协议错误。

失败不推进游标，不清空已有事件。事件取消或来源禁用会阻止后续发送；来源超过可配置的新鲜度上限（初始建议 24 小时）后暂停其推送并显示降级状态，ICS 可继续提供最后快照但注明陈旧。

## 4. 两类输出

### ICS 订阅输出

ICS 是客户端定期拉取的日历资源，刷新频率由客户端决定，不能用来保证分钟级提醒。输出可包含 VALARM，是否执行由客户端决定。[iCalendar](https://datatracker.ietf.org/doc/html/rfc5545)

- 每条订阅绑定一组日历、过滤器、字段可见性和提醒导出策略；只读。
- `GET /feeds/{public_id}/{secret}.ics`，使用可撤销、可轮换的订阅 token，不套浏览器人机挑战。
- `Content-Type: text/calendar; charset=utf-8`，稳定 UID、DTSTAMP、SEQUENCE、LAST-MODIFIED，支持 ETag/304。
- 序列 UID 由内部稳定身份映射生成，源更新不换 UID；不同来源相同 UID 不自动合并。
- 默认输出母事件、EXDATE 和独立例外，保留原有时间语义；携带所需 VTIMEZONE。不能渲染的事件显式报告，不能无声丢弃。
- 事件取消先输出取消标志的墓碑并保留一定窗口，随后清理；必须在目标日历客户端验证取消和删除行为。
- 可选“有限窗口展开”模式必须明确显示截断范围，不宣称为无限完整日历；默认模式不依赖提醒规划的 30 天窗口。
- 订阅 token 验证先于缓存命中；缓存键包含订阅 ID、配置修订和数据修订，响应 `Cache-Control: private, no-cache`，避免共享缓存泄露。

### Bark 与其他主动输出器

- 每个 output 配置定义类型、凭据引用、模板和限流；提醒规则将日历绑定到一个或多个输出。
- Bark 使用 HTTPS POST JSON，不把 device key 拼入日志中的 URL；可以配置自托管 Bark 服务。
- 只有 HTTP 和 Bark 应用响应均成功才记 `accepted`。该状态表示服务接受请求，不代表设备已显示通知。[Bark API](https://github.com/Finb/bark-server/blob/master/docs/API_V2.md)
- 网络超时、429、5xx 可重试，采用带抖动指数退避并尊重 Retry-After；无效凭据等永久错误停止重试。
- 默认最多 8 次尝试、追赶窗口 15 分钟；超过截止时间记 `expired`。均为可配置的初始设计值。
- 不假定 Bark 支持幂等键或设备回执。请求被接受但响应丢失时，重试可能重复；界面显示“结果未知”及重复风险。

ICS 导出器不实现推送接口；它与 Bark 共用事件查询和规则，但分别采用 `FeedRenderer` 与 `PushOutput`。

## 5. 提醒规划与持久化队列

规划器按 UTC 当前时刻展开未来 30 天实例，每 5 分钟延展，并在事件/规则更新后增量重算；最大提前量初设 7 天，窗口至少覆盖最大提前量加扩展周期。每源/规则限制展开数量和计算时长。

全天事件默认在日历配置时区的当天 09:00 提醒；floating 时间必须通过显式日历时区解析。首版提醒规则仅支持相对事件开始的单次偏移与全天时间；完整导入警报结构不表示已支持全部执行语义。

一次逻辑提醒的稳定身份：`event_id + original_recurrence_key + rule_id + output_id`。实例改期仍使用原始 recurrence key。投递任务另带 event/rule/output 的 generation 与计划触发时间。

状态机：

```text
pending -> leased -> accepted
                  -> retry_wait -> leased
                  -> failed_permanent
                  -> expired
pending / retry_wait / leased -> superseded | cancelled
leased --租约超时--> retry_wait（结果可能未知）
```

- SQLite 是任务真相源；内存定时器只负责唤醒。定期查询到期任务，启动时回收过期租约并补偿规划。
- 短事务以 CAS 抢占任务并写 lease_owner、lease_token、lease_until；事务外执行 HTTP，回来按 lease_token 更新结果。租约必须长于请求总超时，长请求需续约。
- 每次发送前检查事件、来源、规则、输出仍有效且 generation 匹配。规划更新作废旧任务，只创建新一代待发任务。
- 已成功的逻辑提醒默认不因改期重发；单独开启“变更通知”才能生成不同类型的新提醒。修改规则同样默认不补发已经成功的逻辑提醒。
- 取消与外部发送之间无法跨系统原子提交。若取消发生在最终检查之后，仍可能收到一次旧提醒；记录时间和修订以供审计。
- 本地唯一键减少重复，外部效果只能提供可重试的至少一次尝试语义，不能承诺 exactly-once。
- 规划水位未追上事件修订时，过期 generation 的任务不可发送；feed 直接读已提交事件，不依赖任务队列。
- 时钟来自可注入 Clock；持久化时间使用 UTC，耗时/心跳使用单调时钟。时钟跳变后重新扫描到期范围，超出追赶窗口不补发。

## 6. 事务边界与存储

采用 SQLite WAL、外键、FULL 同步、小连接池与明确 busy timeout。单进程、单节点持久卷；并发写入靠短事务串行化，不跨网络调用持有写锁。[SQLite WAL](https://sqlite.org/wal.html)

三个原子边界：源事件与 checkpoint；实例/提醒任务与规划水位；投递租约/结果与尝试记录。事件与规划采用持久化修订日志衔接，不用“提交后只发送内存消息”的方式保证执行。

详细表设计、唯一约束和索引见 [事件与存储模型](event-model.md)。若未来需要多节点写入，再替换仓储为 PostgreSQL；当前不预设 SQLite 可水平扩展。

## 7. HTTP 与鉴权边界

| 路由示意 | 作用 | 身份要求 |
| --- | --- | --- |
| `/admin/*`、`/api/admin/v1/*` | 管理页面及配置 | Access 身份 + 应用管理会话；写操作防 CSRF |
| `POST /api/admin/v1/session` | 人机校验后创建会话 | Access JWT + Turnstile 服务端校验 |
| `PUT /api/ingest/v1/events/{uid}` | 按来源提交标准事件 | scoped bearer token + Idempotency-Key |
| `DELETE /api/ingest/v1/events/{uid}` | 显式删除事件 | scoped bearer token + 条件修订 |
| `GET /feeds/{id}/{secret}.ics` | 日历客户端订阅 | 独立只读订阅 token |
| 内部 `/health/live`、`/health/ready` | 运行检查 | 容器内或内部网络，不公开详情 |

API token 绑定 source_id，正文不允许覆盖；事件 external_uid 须与路径一致。幂等键按 token 身份、路由和请求摘要存储，重复键不同正文返回 409；过期保留期明确公布。事件更新使用 If-Match 或预期 revision 防止覆盖；首次写入支持 If-None-Match。语义非法返回 422，冲突 409，响应携带 request_id。

管理 API 在实现阶段提供 OpenAPI，并由契约生成 TypeScript 类型；禁止前后端各自手写不同的时间模型。事件 schema 是结构契约，跨字段与时区校验仍由领域层执行。
