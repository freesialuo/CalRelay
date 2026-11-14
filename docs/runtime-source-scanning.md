# 有界来源周期扫描（#95 / #74 第一批）

`calendar-server::source_worker::{SourceScanner, IcsSourceRunner}` 消费 main 已合并的唯一 `SourceConfigurationRecord/Page`，复用 `HttpSource`、受限 ICS 解析、`synchronize` 和 SQLite 完整快照 CAS。不复制 DTO、迁移、管理 API 或 supervisor。#95 仅关闭此叶子，#74 的手动更新、多 calendar URL 映射、标准 envelope 执行和最终生产组合仍待后续验收。

## 调度与边界

每页沿用配置发现最多100摘要；每轮最多1–16页、1–64次同步、1ms–120s单调准入预算。游标保存在实例中：尝试数耗尽时只推进到已处理的来源，下轮继续，不跳过当前页剩余来源。完整遍历后从头再扫；并发配置变更不提供跨页快照，下轮收敛。缓存容量显式限制1–16384个活跃 pull 来源；满额时先用单个 SQLite 读取快照复核缓存 id，清理已禁用/退休/非 pull/缺失项，保留活跃失败来源的退避。确实超限才返回类型化 Capacity，不能悄悄淘汰失败来源然后忙重试。复核查询使用最多16384个绑定参数且不持久缓存动态语句。单实例顺序执行，无无界任务派生或并发；多扫描器竞争仍由原 CAS 防止迟到提交，不声称分布式协调。

只对 enabled、非退休、pull/ICS/snapshot 配置调用同步；static、push-only、disabled 和 retired 不 fetch。一个 ICS 文档对应一个日历（#172）：配置校验拒绝多日历 ICS 来源；此前保存的遗留多日历行仍返回 Unsupported 并退避、上传返回 409，不任选 calendar、不将局部结果当全源快照。selected 仍是输出选择，不作为是否保存源数据的开关。

进程内期限使用注入 Clock 的单调时间；成功后从操作完成起等待配置周期，失败独立指数退避并封顶。429/5xx 带有效 Retry-After 时，HttpSource 以类型化 `IngestionError::RetryAfter` 传到调度层：下次请求不早于本地退避与上游等待中的较大者，本地退避上限不缩短上游等待；上游等待本身最多采纳7天（最大可配置刷新周期），缺省、无效或溢出的 Retry-After 沿用本地退避。调度等待为1ms–60s且可被停止通知打断；退避基数至少等于扫描等待、上限最多1小时。数量/耗时预算只限制新准入，不中断已准入同步；元数据读取后、容量复核后以及取得 task permit 后均复核截止时间，超限不启动同步并保留当前来源续点。实际网络/解析/提交继续受既有 RemoteFetchLimits、IcsLimits、SyncLimits 和 SQLite busy 预算限制；HTTP仍在事务外。根装配必须让真实单次同步与等待预算适配 supervisor stall_timeout，不能用等待 timer 伪造进度。

重启从持久 last_verified_at 重建剩余周期：过去核验时间用 max(0, interval-age)，未来UTC核验时间保守等待一个完整周期；没有可信核验则在有界轮次内首次同步。进程内 UTC 前后跳不改变期限。每次同步得到结果（成功、未修改或外部失败）后，在同一次任务准入内于 `source_sync_status`（迁移 `0015`）按来源覆盖一行，停止期间完成的同步结果也会保存：配置修订、尝试完成 UTC 时间、结果枚举、连续失败次数与最早下次请求 UTC 时间 `next_allowed_at`（即手动刷新下限 `not_before`）。不保存错误正文、端点、凭据或事件内容。重启后首次见到该来源时，只采用当前配置修订的状态：恢复失败次数，`not_before` 取持久化等待的剩余部分，到期时间不早于它和按失败次数计算的本地退避剩余部分。UTC 早于尝试时间时保守等待完整跨度；跨度上限7天，不会永久停摆；UTC 前跳越过边界则立即允许。旧修订的状态不约束新配置。仍不假称 exactly-once GET。

## fence、鉴权与进度

配置 record 的 source_revision 与配置 JSON 来自同一次读取；构建 adapter 时 pin 该 revision。既有 synchronize 再读 SourceState，如果不匹配，在网络前返回 Conflict。网络期间修改、禁用、退休继续由 commit_snapshot/verify_unchanged 的最终 source CAS 拒绝，不能把旧配置配上新 revision 重放。已准入的外部 GET 无法跨数据库原子撤回；不把配置变更后已在途的请求误称“从未请求”。失败/超时/冲突不清空上次快照或推进成功核验。

只读 `Storage::source_authentication` 从既有 immutable credential.kind 选择 None/Basic/Bearer，其他类型/缺失引用 fail closed。调度器不读取密文/秘密；实际 SecretProvider、版本与 HTTPS/DNS/SSRF/重定向/字节限额继续由原 HttpFetcher 执行。conditional_read=false 清除传给 adapter 的验证器并拒绝304；启用时复用原配置绑定 checkpoint 与完整快照证明。自动快照使用 DeletionPolicy::Limit(100)，保留已有清空审批/删除阈值，不自动批准大批删除。

每次发现、配置读取、单源同步分别经过 Services::run_task；停止后无新准入，已准入的有界同步可保存结果，外层有限 drain 仍可取消。SourceProgress 只由真实查询/同步/外部结果产生；Scanned 和 Synchronized 携带当前缓存中是否仍有外部降级，单源成功不会清除另一来源的失败。存储故障返回上层，外部失败持续隔离重试；本模块不退出/重启进程，不操作 Docker，不解除恢复 pause/cutoff。

## 手动刷新（#150）

`POST /api/admin/v1/sources/{source_id}/refresh`（空正文，沿用 Access/会话/Origin/CSRF/一次性写 nonce）只为启用、未退休的 pull/ICS/snapshot 来源在 `source_refresh_requests`（迁移 `0014`）记录一个待处理请求并返回 202；重复请求合并为同一行。manual-static、push-only、禁用或退休来源返回 409，不产生假轮询；未知来源 404。202 只表示请求已记录，不表示同步已执行或成功。

扫描器在下一轮读取该来源时消费请求：跳过配置周期和本地失败退避立即同步，但不早于 `not_before`——上次完成后的本地退避基数（生产 1 秒）与上游 Retry-After 中的较大者；被推迟的请求保留。同步仍走既有 revision fence、条件读取和轮次预算。请求在同步得到结果（成功或外部失败）后按 `sequence` 清除，本轮未获准入的请求保留；同步期间到达的新请求序号不同，会保留到下一轮；针对旧配置修订的请求直接丢弃。请求与 `not_before` 均持久化，重启后的手动刷新同样不早于上游 Retry-After。该表只含来源 ID、配置修订、序号和时间，不写审计表，不含端点、凭据或事件内容。管理界面入口见[来源管理前端](source-web.md)（#152）。

## 验证与未交付

17项集成测试使用真实 SQLite、真实配置 CRUD、测试专用 RemoteFetcher 到本地模拟 HTTP、注入 UTC/单调时钟和信号 barrier。覆盖周期/304/无条件读取、失败保留/独立退避、手动刷新（跳过周期、本地下限与 Retry-After 边界、合并、重启保留、旧修订丢弃、非 pull 拒绝）、429/503 Retry-After 的边界前不请求/边界可请求/不低于本地退避/7天上限且其他来源照常、重启和未来核验时间、持久等待恢复（Retry-After 与手动刷新跨重启、失败次数续算、UTC 后跳、7天上限、旧修订忽略、停止期间完成的结果按完成时间保存）、101来源分页、attempt/cache/单调预算、旧 record 网络前拒绝、在途修改/退休、停止排空、HTTP超时与非秘密 credential kind；另覆盖排序靠后的失效缓存回收且保留活跃失败退避、读取阶段耗尽预算后不准入/下轮续扫，以及16384参数查询边界。实际 HttpFetcher 的 TLS/SSRF/认证矩阵在 workspace 原有测试回归；没有生产网络或 loopback 放行开关。

受控去掉 revision pin 与周期准入门槛，分别触发旧配置错误通过及提前轮询的行为断言失败；恢复后通过。自动审核两项 P2 也先观察有效 Red：替换后错误 Capacity；读取耗尽预算后实际(1,Complete)/预期(0,Deadline)，修复后通过。夹具遗漏必需 DTSTAMP、退休保留旧核验时间的测试预期和语法错误不计作需求 Red。未运行长时 fuzz/形式证明/真实上游互操作。

该模块只提供扫描器；标准 binary 的实际注册、安全 bootstrap 与预算见[生产运行时](production-runtime.md)，复用已合并 #71/#96/#97/#104。#74 的其他协议能力与整体 AC 仍保留。

参考：[已有协调器与网络安全边界](source-coordinator.md)、[RFC 9110 §10.2.3 Retry-After](https://www.rfc-editor.org/rfc/rfc9110.html#section-10.2.3)、[来源配置契约](source-configuration.md)、[Tokio watch](https://docs.rs/tokio/1.54.1/tokio/sync/watch/struct.Receiver.html) 的订阅/取消安全语义；通过 Exa 核对 [SQLx 0.9.0 QueryBuilder](https://docs.rs/sqlx/0.9.0/sqlx/struct.QueryBuilder.html) 的绑定参数、SQLite默认32766参数限制与动态语句 persistent(false)，实际16384边界由当前 bundled SQLite 集成测试验证。
