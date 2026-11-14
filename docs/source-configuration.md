# 来源配置契约与分阶段交付

[#80](https://github.com/freesialuo/CalRelay/issues/80) 是 [#73](https://github.com/freesialuo/CalRelay/issues/73) 第一批；保留 #73 AC1–AC6、#35 全部 AC，父 Issue 不能因本批合并关闭。唯一契约 owner 维护 application DTO、schema、管理 OpenAPI 与迁移，后续 UI/runtime/aggregation 复用这些类型。本页描述 #80 当批范围；后续 #96/#112 已装配安全 admin/provider 与真实周期工作器，#94/#109 已提供 UI，#110 已提供镜像。当前状态见[首版对账](roadmap.md#首个-alpha-对账2026-10-26)；插件执行与真实账号操作不属于本页交付。

## 可实现的最小配置 API

Rust 权威为 `calendar-application::configuration::{SourceConfigurationV1, SourceConfigurationRecord, CalendarMappingV1, SourceCapabilitiesV1}`；结构 schema 为 `schemas/source-configuration-v1.schema.json`，示例为 `examples/source-configuration-v1.json`。业务校验进一步限制能力/模式/日历归属/时区/URL，不把 JSON Schema 通过当作可启用。 UI 类型生成可运行 `cargo run -p calendar-server --example export_openapi --locked` 导出与 HTTP 相同的 OpenAPI；该命令不读取 env、不打开 DB 或监听器。

| 请求 | 条件与结果 |
| --- | --- |
| GET `/api/admin/v1/sources?after=ID` | Access + session；最多100条无配置/凭据摘要，含退休状态；next_after 非空时续页；按稳定 id 排序，非跨并发快照 |
| GET `/api/admin/v1/sources/{source_id}` | Access + session；200 record + ETag，404 未托管；退休 record 仍可审计读取 |
| PUT 同路径 | 严格配置正文；创建 `If-None-Match: *` 返回 201，必须 enabled=false；更新 `If-Match: "N"` 返回 200；ETag 是 config_revision，不是事件或 source revision |
| DELETE 同路径 | 空正文，`If-Match: "N"`；204 逻辑退休，永久禁止同 id 重建/启用；重复删除/旧 revision 409 |

沿用 #34 Access JWT、Secure HttpOnly SameSite session、精确 Origin、CSRF 和一次 write nonce。失败鉴权不写业务数据；已经授权的 unsafe 请求即使业务失败也消费 nonce，客户端读取 `X-Admin-Write-Nonce` 后重试。GET/写响应均 no-store。无条件写、弱/重复/同时携带的条件头、未知字段/版本、非法能力为 422（JSON 提取层的语法错误为 400）；stale CAS/身份重映射为 409。沿用全局有界错误码与 request_id；持久错误不回显配置/凭据。管理来源正文额外限制 64 KiB，全局部署预算可更小；原会话登录仍限制 4 KiB。

id 为 1–128 ASCII 字母/数字/`_`/`-`；name 1–256 UTF-8 bytes、非空白无控制字符；1–256 个日历映射；ICS 来源（pull 与 manual-static）恰好一个：一个 ICS 文档对应一个日历，无论是否启用，多映射一律拒绝（#172）。calendar id 全库唯一、external_id 源内唯一；跨源同 EventV1.external_uid 仍由原 `(source_id, external_uid)` 隔离。已有 calendar id/external_id 不可重映射或移除，取消选择用 selected=false，保留事件/历史引用。timezone 只允许有效 IANA TZID 或 null。新日历可追加。发现接口使用 `SourceConfigurationSummary` / `SourceConfigurationPage`，不导出完整配置或凭据引用。

| mode | 本批允许配置与 freshness |
| --- | --- |
| pull | ICS adapter，HTTPS endpoint，不允许 URL userinfo/query/fragment；凭据只能已有 credential_id 引用；60 秒–7 天周期、显式 60 秒–30 天 SLA，snapshot/upsert，conditional_read 可选，不宣称 delta/delete/pagination 支持 |
| manual-static | 无 endpoint/凭据/周期/SLA，离线正常；ICS 仅 snapshot/upsert，standard-calendar 可声明 snapshot/delta/upsert/delete |
| push-only | standard-calendar，无 endpoint/凭据/周期，SLA 可选；无 SLA 时离线不自动判陈旧；snapshot/delta 至少一种、upsert 必须，delete 依赖 delta |

能力描述源侧协议承诺，不表示本服务已挂载 envelope 批次入口。pull endpoint 保存不进行网络 I/O；#74 在使用时继续执行 #28 DNS/重定向/SSRF/凭据策略，配置验证不能替代远程安全校验。刷新/规划/投递工作器 #74/#75 必须读取此 DTO，不从 env 构造另一套业务配置。

## 持久化与在途 fence

唯一新增迁移 `0011_source_configuration.sql`；当批 main 为 0010，#70 使用 0008（现已合并），不复制它的 schema。旧来源保留 config_json/事件/启用状态与 config_revision=0；GET 返回 404，不猜测转换未知旧配置，PUT 不允许覆盖。后续明确导入迁移需单独验收。

首次托管来源 config/source revision 均为 1。BEGIN IMMEDIATE 短事务校验 CAS、凭据引用、稳定日历身份，写配置/日历与摘要审计；任一步失败整体回滚。无网络或 secret provider 调用。每次 PUT（包括同值写）config_revision 与 source revision 均 +1，清除 checkpoint/snapshot proof/last_verified，强制新拉取而不将旧内容当作新配置的可信基线；事件本体仍保留。重命名也保守推进 fence；#75 必须扫描当前 source revision 与 planning_watermarks 的差异，按新 revision 调用 #23 reconcile；它会为 superseded 的未 accepted 提醒建立新 generation，已 accepted 不重发。pull 重新核验后再发送，manual/push 的无 SLA 策略由 #74/#75 使用 DTO 装配；生产循环现由 #112 装配；本批库测试本身仍不代表完整变更竞争系统验收。#29 commit_sync/verify_unchanged、#23 reconcile 现有 source revision 比较拒绝旧请求；配置 ETag 独立于事件 ingest 的数据 revision。

每次修改把该源 pending/retry_wait/leased jobs 标 superseded；退休标 cancelled、禁用 source、撤销 producer token 与单源 feed_subscriptions 并增加其 revision。保留 lease 与 attempt：已 prepare/send 的旧请求可能仍 accepted，#24 finish 记录实际 accepted outcome + superseded/cancelled disposition，并保留 logical_deliveries.accepted_at，不能谎称外部撤回。已 accepted jobs、event tombstone、logical dedup、attempt/audit/history 不物理删除；本批无自动清理，未来保留期策略不得让 accepted 重放。

日历 selected 参与现有 planner 输入筛选，更新 source revision 先使旧水位和任务不可用。后续 #76/#101 多源 feed 已将 config_revision/selected/retired 和 source revision 向量纳入鉴权前后 fence/ETag；这是后续已合并能力，不以本批测试冒充其缓存验收。

## 标准源 envelope v1 冻结边界

`schemas/standard-calendar-source-v1.schema.json` 是下一批执行入口的结构契约，引用现有 EventV1 urn，不复制模型。当前只执行 #30 的 `/api/ingest/v1/sources/{source_id}/events/{uid}` PUT/DELETE，既有非托管来源的认证、If-Match/If-None-Match、token-wide Idempotency-Key、receipt TTL 及预算原样兼容。新托管来源只允许 push-only + standard-calendar 的已声明操作使用机器 API；在回执查询前重新核对模式/能力/启用/退休，配置收窄不能通过旧幂等回执放行。旧单事件入口没有 config CAS 请求头；同一允许操作的普通配置更新仍按原 Event revision CAS，完整 config/source/checkpoint CAS 由后续 envelope 入口承载，不冒充已完成 #73 AC5。#30 本身没有机器批次端点；单页快照与增量批次由 #174/#176 接入（见下文）。不能把 envelope 发往旧单事件端点；它会按未知形状拒绝。

后续 envelope 入口应为独立版本路径（实现前在 OpenAPI 明示），同一个 source-scoped token 的授权先于幂等回执；认证上下文注入 source_id，正文任何 source_id/权限/内部 context 均是未知字段拒绝。protocol_version 固定 `standard-calendar-source/v1`；batch_id 是 idempotency key 的可审计关联而非来源身份。upsert 携带 calendar_id + 原 EventV1；delete 携带 external_uid + 正 revision。所有 upsert/delete 授权、结构/领域/日历归属检查必须整批完成后短事务提交。

每批必须包含 base_source_revision、base_config_revision、base_checkpoint、proposed_checkpoint、complete=true、pages、collection_tag、operations。snapshot 的 base_checkpoint=null，只含 upsert；分页最多32页，超过1页必须同一 collection_tag，协调器收齐才组装单 envelope。不能信任 producer 单称 complete；trusted adapter/授权的 snapshot producer 仍须校验完整性及删除阈值。snapshot 缺失删除超过100条或清空非空来源需要单独管理批准的新配置 revision；不得把 producer 布尔值当 approval。delta 必须匹配已持久 checkpoint（包括合法的 null 基线）及源/config revision，只认显式 delete；失配409、无完整基线409并要求 snapshot，失败不推进 checkpoint。

后续新批次摘要冻结为 SHA-256(JCS RFC 8785 的完整 envelope UTF-8)，服务端计算，不相信客户端提供的 digest；JSON 重复键必须在解析时拒绝，非 JCS 可表示数值拒绝。操作数组顺序有意义但重复 UID 拒绝；null/省略不等价（envelope 所有字段必填）。receipt 分区为认证 principal + source + versioned route + Idempotency-Key，24小时保留、全库100000未过期记录容量（沿用 IngestPolicy 部署范围）；同 key 异摘要409，容量满503不淘汰有效项，同 key 同摘要返回原回执。撤销/到期/禁用/退休即时拒绝，不能通过 receipt 绕过。记录过期后必须重新核对 base config/source/checkpoint 与 delete/event CAS，不能覆盖新修订。#30 既有摘要算法/token-wide key 继续不变，不能无迁移合并新 receipt namespace。

### 生产者令牌（#178）

`GET /api/admin/v1/sources/{id}/tokens` 列出该来源令牌元数据（ID、权限、到期、撤销、签发时间）：全部有效令牌在前（至多 16 个，始终在页内），其后为最新的历史记录，每页最多 100 条并以 `more` 提示更早历史，从不返回密钥或摘要。`POST /api/admin/v1/sources/{id}/tokens`（正文 `{can_upsert,can_delete,expires_at|null}`）只对受管、未退休的 push-only standard-calendar 来源签发，至少一项权限，到期须晚于当前时间；服务端生成 `pt_` 前缀 ID 与 256 位密钥，密钥只在 201 响应中返回一次，库内只存摘要。每来源最多 16 个未撤销且未过期的令牌，超限 409。`POST /api/admin/v1/sources/{id}/tokens/{token_id}/revoke` 立即撤销，重复撤销无副作用。签发与撤销沿用管理写入保护（Access、会话、Origin/CSRF、一次性写 nonce），并写入 `producer_token_audit`（令牌 ID、来源、动作、管理员摘要、时间，迁移 `0017`）；退休来源仍撤销其全部令牌。快照批次需要同时具备 upsert 与 delete 权限的令牌。

### 超限删除批准（#186）

`POST /api/admin/v1/sources/{id}/deletion-approval`（空正文，沿用管理写入保护）为已启用、未退休的 pull 或 push-only 受管来源记录一次批准，绑定当前配置修订与来源修订，24 小时内有效，返回 `{source_id,config_revision,source_revision,expires_at}`；其他来源 409，不存在 404，重复批准覆盖为最新。匹配的批准让下一次快照提交（push 批次或周期拉取）可以删除超过 100 个事件或清空非空来源；任何提交都会推进来源修订，因此批准至多作用于下一次提交，生产者无法自行批准。每次批准写入 `deletion_approval_audit`（来源、修订、管理员摘要、时间；迁移 `0018`）。manual-static ICS 上传仍用逐次 `approve_deletions`。

### 已执行的叶子（#174 快照、#176 增量）

`POST /api/ingest/standard-calendar/v1/sources/{source_id}/batches` 执行单页批次：`pages=1`、`collection_tag=null`；多页返回 422（尚未支持），结构契约不变。来源须为已启用、未退休的 push-only + standard-calendar，快照须声明 snapshot，增量须声明 delta、含 delete 操作时还须声明 delete，否则 409。bearer 须绑定该来源；快照会删除缺失事件，须同时具备 upsert 与 delete 权限，增量按所含操作类型分别需要对应权限，否则 401；必须带 `Idempotency-Key`。解析拒绝重复键、小数与超出 ±(2^53−1) 的整数（本协议没有小数字段，安全整数在 JCS 下按十进制原样表示），随后执行 envelope 与 EventV1 Schema、类型化及领域校验。摘要为 SHA-256(JCS)，由服务端计算。

令牌、来源状态、回执与提交在同一写事务内：先验证令牌与来源，再查回执（同 key 同摘要返回原回执，异摘要 409），两条机器路由的未过期回执合计不超过 `IngestPolicy.max_records`，任一路由满额都返回 503 且不淘汰；然后核对 `base_config_revision`、`base_source_revision`（失配 409）。快照：删除超过 100 条或清空非空来源返回 409 且不变，除非管理员已为当前配置与来源修订记录一次性批准（#186，见下文）。增量：须已有完整快照基线、`base_checkpoint` 等于已持久 checkpoint（含 null），每个 delete 的 `expected_event_revision` 须等于事件当前修订（已删除且修订相符为无变化），未出现的事件不删除；任一不符 409 且不推进 checkpoint。上游 `source_sequence` 回退 409。成功时写入 `proposed_checkpoint` 并返回 `{source_id,batch_id,source_revision,changed_events}`。回执表 `envelope_receipts`（迁移 `0016`）以 token + key 摘要为主键，只存摘要与计数，保留期沿用 `IngestPolicy.retention_ms`。

`GET /api/ingest/standard-calendar/v1/sources/{source_id}/state` 对声明 snapshot 或 delta 的 push-only 来源，用同一来源令牌（任一写权限）返回 `{source_id,config_revision,source_revision,checkpoint,has_snapshot}`，供 producer 填写 base 字段；不返回事件、端点、凭据或配置正文，鉴权与来源拒绝同上。

预算：每个 envelope 请求同时受部署 max_request_bytes 与32MiB同步总字节上限约束（取更小），最多10000操作、32页、256日历、60秒同步时限、单源一个提交胜者；EventV1 单字段/时区/重复展开仍受原 schema/domain/SyncLimits/PlanningLimits/IcsLimits 约束，不新增无界展开。结构/版本/未知操作422，认证401，授权作用域拒绝按 #30 401，CAS/基线/重放冲突409，大规模删除待审409，预算413或同步 budget/容量503；均有 request_id，无事件/凭据正文日志。多页、删除批准与浏览器 producer 语义仍待后续叶子；单页快照与增量由 #174/#176 的 HTTP+SQLite 集成测试覆盖，不把 schema 测试当作服务器已支持。

浏览器只申请明确来源域权限；后台隔离保存 scoped token，不进入页面 localStorage/DOM/内容脚本/URL；仅向精确配置 ingest origin 发请求，不携带管理 Cookie。服务继续无通配 CORS；确切 extension origin/方法/header 需单独受控策略，CORS 不能替代 bearer。主服务不下载/运行任意 producer，插件进程与浏览器安装/权限/真实账号不在本批范围。

## 验证与余项

#80 A1/A3：真实 Axum HTTP + SQLite、CSRF/Origin/session/机器 token 负例、配置双写 CAS、旧 fetch 拒绝、失败无副作用、accepted/取消竞争、重开与历史迁移/备份恢复。A2：schema/兼容文档与拒绝样例，不声称 envelope 执行通过。A4：真实 binary 子进程文件启动/SIGTERM/旧 env 诊断，部署配置严格字段测试。完整 gate 按 CONTRIBUTING。

仍未覆盖 #73：规则/输出/订阅 CRUD 与诊断查询、机器 envelope 执行与摘要/重放状态、多源缓存 fence、生产安全管理装配、UI与周期/发送系统旅程。后续批次继续同一 owner 和契约，不关闭父项。

参考实际使用：[SQLite BEGIN IMMEDIATE 与单写者事务](https://www.sqlite.org/lang_transaction.html)、[RFC 8785 JCS](https://www.rfc-editor.org/rfc/rfc8785)、[Axum stateful middleware](https://docs.rs/axum/0.8.6/axum/middleware/fn.from_fn_with_state.html)。后两者分别为后续摘要规范与既有管理 guard 装配边界，不引入新的协议库。

本批实测证据（云端合成数据）：`cargo test --workspace --all-features --locked` 255 tests、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、`cargo fmt --all --check`、Python unittest 20 tests、repository checker 与 diff check 通过。有效 Red：旧 binary 无部署文件升级诊断；受控移除配置写的 source revision 推进后，`cas_rename_restart_retirement_and_atomic_failure` 因旧 fetch 错误获准而失败，恢复实现后配置/规划专项通过。没有运行 envelope HTTP 执行、浏览器/UI、生产 worker、长 fuzz 或形式证明，不能从这些测试声称对应功能已完成。

后续规则/Bark输出持久配置见[提醒配置](reminder-configuration.md)（独立叶子#92），不改变本页#80历史交付范围；#73的订阅/envelope/系统集成验收继续保留。
