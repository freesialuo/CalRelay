# 来源配置契约与分阶段交付

[#80](https://github.com/freesialuo/CalRelay/issues/80) 是 [#73](https://github.com/freesialuo/CalRelay/issues/73) 第一批；保留 #73 AC1–AC6、#35 全部 AC，父 Issue 不能因本批合并关闭。唯一契约 owner 维护 application DTO、schema、管理 OpenAPI 与迁移，后续 UI/runtime/aggregation 复用这些类型。没有前端、周期工作器、插件执行、生产 Docker 管理或真实账号操作；默认 binary 仍 `admin=None`，只有显式 `Server::with_admin` / `AdminState` 装配才挂载管理路由。

## 可实现的最小配置 API

Rust 权威为 `calendar-application::configuration::{SourceConfigurationV1, SourceConfigurationRecord, CalendarMappingV1, SourceCapabilitiesV1}`；结构 schema 为 `schemas/source-configuration-v1.schema.json`，示例为 `examples/source-configuration-v1.json`。业务校验进一步限制能力/模式/日历归属/时区/URL，不把 JSON Schema 通过当作可启用。

| 请求 | 条件与结果 |
| --- | --- |
| GET `/api/admin/v1/sources?after=ID` | Access + session；最多100条无配置/凭据摘要，含退休状态；next_after 非空时续页；按稳定 id 排序，非跨并发快照 |
| GET `/api/admin/v1/sources/{source_id}` | Access + session；200 record + ETag，404 未托管；退休 record 仍可审计读取 |
| PUT 同路径 | 严格配置正文；创建 `If-None-Match: *` 返回 201，必须 enabled=false；更新 `If-Match: "N"` 返回 200；ETag 是 config_revision，不是事件或 source revision |
| DELETE 同路径 | 空正文，`If-Match: "N"`；204 逻辑退休，永久禁止同 id 重建/启用；重复删除/旧 revision 409 |

沿用 #34 Access JWT、Secure HttpOnly SameSite session、精确 Origin、CSRF 和一次 write nonce。失败鉴权不写业务数据；已经授权的 unsafe 请求即使业务失败也消费 nonce，客户端读取 `X-Admin-Write-Nonce` 后重试。GET/写响应均 no-store。无条件写、弱/重复/同时携带的条件头、未知字段/版本、非法能力为 422（JSON 提取层的语法错误为 400）；stale CAS/身份重映射为 409。沿用全局有界错误码与 request_id；持久错误不回显配置/凭据。管理来源正文额外限制 64 KiB，全局部署预算可更小。

id 为 1–128 ASCII 字母/数字/`_`/`-`；name 1–256 UTF-8 bytes、非空白无控制字符；1–256 个日历映射。calendar id 全库唯一、external_id 源内唯一；跨源同 EventV1.external_uid 仍由原 `(source_id, external_uid)` 隔离。已有 calendar id/external_id 不可重映射或移除，取消选择用 selected=false，保留事件/历史引用。timezone 只允许有效 IANA TZID 或 null。新日历可追加。发现接口使用 `SourceConfigurationSummary` / `SourceConfigurationPage`，不导出完整配置或凭据引用。

| mode | 本批允许配置与 freshness |
| --- | --- |
| pull | ICS adapter，HTTPS endpoint，不允许 URL userinfo/query/fragment；凭据只能已有 credential_id 引用；60 秒–7 天周期、显式 60 秒–30 天 SLA，snapshot/upsert，conditional_read 可选，不宣称 delta/delete/pagination 支持 |
| manual-static | 无 endpoint/凭据/周期/SLA，离线正常；ICS 仅 snapshot/upsert，standard-calendar 可声明 snapshot/delta/upsert/delete |
| push-only | standard-calendar，无 endpoint/凭据/周期，SLA 可选；无 SLA 时离线不自动判陈旧；snapshot/delta 至少一种、upsert 必须，delete 依赖 delta |

能力描述源侧协议承诺，不表示本服务已挂载 envelope 批次入口。pull endpoint 保存不进行网络 I/O；#74 在使用时继续执行 #28 DNS/重定向/SSRF/凭据策略，配置验证不能替代远程安全校验。刷新/规划/投递工作器 #74/#75 必须读取此 DTO，不从 env 构造另一套业务配置。

## 持久化与在途 fence

唯一新增迁移 `0011_source_configuration.sql`；main 0010，未合并 #70 保留 0008，不复制它的 schema。旧来源保留 config_json/事件/启用状态与 config_revision=0；GET 返回 404，不猜测转换未知旧配置，PUT 不允许覆盖。后续明确导入迁移需单独验收。

首次托管来源 config/source revision 均为 1。BEGIN IMMEDIATE 短事务校验 CAS、凭据引用、稳定日历身份，写配置/日历与摘要审计；任一步失败整体回滚。无网络或 secret provider 调用。每次 PUT（包括同值写）config_revision 与 source revision 均 +1，清除 checkpoint/snapshot proof/last_verified，强制新拉取而不将旧内容当作新配置的可信基线；事件本体仍保留。重命名也保守推进 fence；#75 必须扫描当前 source revision 与 planning_watermarks 的差异，按新 revision 调用 #23 reconcile；它会为 superseded 的未 accepted 提醒建立新 generation，已 accepted 不重发。pull 重新核验后再发送，manual/push 的无 SLA 策略由 #74/#75 使用 DTO 装配；当前无生产循环，不能把本库 API 的作废当作完整产品重规划已交付。#29 commit_sync/verify_unchanged、#23 reconcile 现有 source revision 比较拒绝旧请求；配置 ETag 独立于事件 ingest 的数据 revision。

每次修改把该源 pending/retry_wait/leased jobs 标 superseded；退休标 cancelled、禁用 source、撤销 producer token 与单源 feed_subscriptions 并增加其 revision。保留 lease 与 attempt：已 prepare/send 的旧请求可能仍 accepted，#24 finish 记录实际 accepted outcome + superseded/cancelled disposition，并保留 logical_deliveries.accepted_at，不能谎称外部撤回。已 accepted jobs、event tombstone、logical dedup、attempt/audit/history 不物理删除；本批无自动清理，未来保留期策略不得让 accepted 重放。

日历 selected 参与现有 planner 输入筛选，更新 source revision 先使旧水位和任务不可用。未来 #76 多源 feed 必须将 config_revision/selected/retired 和 source revision 向量纳入鉴权前后 fence/ETag；本批未引入多源订阅或缓存。#70 仍未合并，不以本批测试冒充它的缓存验收。

## 标准源 envelope v1 冻结边界

`schemas/standard-calendar-source-v1.schema.json` 是下一批执行入口的结构契约，引用现有 EventV1 urn，不复制模型。当前只执行 #30 的 `/api/ingest/v1/sources/{source_id}/events/{uid}` PUT/DELETE，既有非托管来源的认证、If-Match/If-None-Match、token-wide Idempotency-Key、receipt TTL 及预算原样兼容。新托管来源只允许 push-only + standard-calendar 的已声明操作使用机器 API；在回执查询前重新核对模式/能力/启用/退休，配置收窄不能通过旧幂等回执放行。旧单事件入口没有 config CAS 请求头；同一允许操作的普通配置更新仍按原 Event revision CAS，完整 config/source/checkpoint CAS 由后续 envelope 入口承载，不冒充已完成 #73 AC5。此 PR 没有新增机器批次端点、receipt 表或声称 snapshot/delta 已接入 HTTP。不能把 envelope 发往旧单事件端点；它会按未知形状拒绝。

后续 envelope 入口应为独立版本路径（实现前在 OpenAPI 明示），同一个 source-scoped token 的授权先于幂等回执；认证上下文注入 source_id，正文任何 source_id/权限/内部 context 均是未知字段拒绝。protocol_version 固定 `standard-calendar-source/v1`；batch_id 是 idempotency key 的可审计关联而非来源身份。upsert 携带 calendar_id + 原 EventV1；delete 携带 external_uid + 正 revision。所有 upsert/delete 授权、结构/领域/日历归属检查必须整批完成后短事务提交。

每批必须包含 base_source_revision、base_config_revision、base_checkpoint、proposed_checkpoint、complete=true、pages、collection_tag、operations。snapshot 的 base_checkpoint=null，只含 upsert；分页最多32页，超过1页必须同一 collection_tag，协调器收齐才组装单 envelope。不能信任 producer 单称 complete；trusted adapter/授权的 snapshot producer 仍须校验完整性及删除阈值。snapshot 缺失删除超过100条或清空非空来源需要单独管理批准的新配置 revision；不得把 producer 布尔值当 approval。delta 必须匹配已持久 checkpoint（包括合法的 null 基线）及源/config revision，只认显式 delete；失配409、无完整基线409并要求 snapshot，失败不推进 checkpoint。

后续新批次摘要冻结为 SHA-256(JCS RFC 8785 的完整 envelope UTF-8)，服务端计算，不相信客户端提供的 digest；JSON 重复键必须在解析时拒绝，非 JCS 可表示数值拒绝。操作数组顺序有意义但重复 UID 拒绝；null/省略不等价（envelope 所有字段必填）。receipt 分区为认证 principal + source + versioned route + Idempotency-Key，24小时保留、全库100000未过期记录容量（沿用 IngestPolicy 部署范围）；同 key 异摘要409，容量满503不淘汰有效项，同 key 同摘要返回原回执。撤销/到期/禁用/退休即时拒绝，不能通过 receipt 绕过。记录过期后必须重新核对 base config/source/checkpoint 与 delete/event CAS，不能覆盖新修订。#30 既有摘要算法/token-wide key 继续不变，不能无迁移合并新 receipt namespace。

预算：每个 envelope 请求同时受部署 max_request_bytes 与32MiB同步总字节上限约束（取更小），最多10000操作、32页、256日历、60秒同步时限、单源一个提交胜者；EventV1 单字段/时区/重复展开仍受原 schema/domain/SyncLimits/PlanningLimits/IcsLimits 约束，不新增无界展开。结构/版本/未知操作422，认证401，授权作用域拒绝按 #30 401，CAS/基线/重放冲突409，大规模删除待审409，预算413或同步 budget/容量503；均有 request_id，无事件/凭据正文日志。以上执行语义仍待下一批集成测试，不把 schema 测试当作服务器已支持。

浏览器只申请明确来源域权限；后台隔离保存 scoped token，不进入页面 localStorage/DOM/内容脚本/URL；仅向精确配置 ingest origin 发请求，不携带管理 Cookie。服务继续无通配 CORS；确切 extension origin/方法/header 需单独受控策略，CORS 不能替代 bearer。主服务不下载/运行任意 producer，插件进程与浏览器安装/权限/真实账号不在本批范围。

## 验证与余项

#80 A1/A3：真实 Axum HTTP + SQLite、CSRF/Origin/session/机器 token 负例、配置双写 CAS、旧 fetch 拒绝、失败无副作用、accepted/取消竞争、重开与历史迁移/备份恢复。A2：schema/兼容文档与拒绝样例，不声称 envelope 执行通过。A4：真实 binary 子进程文件启动/SIGTERM/旧 env 诊断，部署配置严格字段测试。完整 gate 按 CONTRIBUTING。

仍未覆盖 #73：规则/输出/订阅 CRUD 与诊断查询、机器 envelope 执行与摘要/重放状态、多源缓存 fence、生产安全管理装配、UI与周期/发送系统旅程。后续批次继续同一 owner 和契约，不关闭父项。

参考实际使用：[SQLite BEGIN IMMEDIATE 与单写者事务](https://www.sqlite.org/lang_transaction.html)、[RFC 8785 JCS](https://www.rfc-editor.org/rfc/rfc8785)、[Axum stateful middleware](https://docs.rs/axum/0.8.6/axum/middleware/fn.from_fn_with_state.html)。后两者分别为后续摘要规范与既有管理 guard 装配边界，不引入新的协议库。

本批实测证据（云端合成数据）：`cargo test --workspace --all-features --locked` 254 tests、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、`cargo fmt --all --check`、Python unittest 20 tests、repository checker 与 diff check 通过。有效 Red：旧 binary 无部署文件升级诊断；受控移除配置写的 source revision 推进后，`cas_rename_restart_retirement_and_atomic_failure` 因旧 fetch 错误获准而失败，恢复实现后配置/规划专项通过。没有运行 envelope HTTP 执行、浏览器/UI、生产 worker、长 fuzz 或形式证明，不能从这些测试声称对应功能已完成。
