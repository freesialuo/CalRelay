# 只读订阅权限、缓存与隐私（#32）

`GET /feeds/{public_id}/{secret}.ics`（同样支持 HEAD）只使用独立的 feed capability；不要求管理会话、Access JWT 或 Turnstile。路由位于 `calendar-server/src/feed.rs`，存储边界位于 `calendar-storage/src/feed.rs`。本项不实现管理配置 HTTP API，也不修改机器写入或管理认证语义。

## 持久化授权与有限 scope

沿用 #26 的 256-bit 随机 token、用途分离 SHA-256 摘要、过期、持久化撤销及 CAS 轮换；明文仅签发/轮换时返回一次。到期等号失效，时间为 UTC Unix 毫秒。每次请求检查 public ID、feed 用途摘要、期限、撤销、source enabled、完整 policy 以及 calendar 归属。未知 scope 统一拒绝，不能降为整 source。

[scope 契约](../schemas/feed-scope-v1.schema.json)兼容历史 `{}`：选择绑定 source 的全部 calendars，保留 renderer 已支持的全部字段。不会在升级时自动将历史订阅收窄或迁移为 v1。受限订阅使用显式对象：

```json
{"version":1,"calendar_ids":["internal-calendar-id"],"visible_fields":["summary","location"]}
```

- `version` 只能为整数 1，两个数组必填；未知键、未知枚举、null、重复项、未知版本及数组形状的 scope 都拒绝。scope 最多 65536 UTF-8 bytes，calendar 最多 256 个，每个 ID 为 1–256 UTF-8 bytes，字段最多五种。
- `calendar_ids` 是绑定 source 内部 calendar ID 的显式白名单。空数组表示空 VCALENDAR；缺失或属于其他 source 的 ID 使整个能力失效，不能忽略该项。过滤同时作用于活动事件和删除墓碑，不执行时间窗口、内容表达式或跨 source 合并。
- `visible_fields` 只允许 summary（EventV1.title）、description、location、url、categories。空数组使用固定 `SUMMARY:Busy`，移除其余四项；不复用源标题作为占位。投影同时清除 recurrence override patch 中对应字段，防止继承/覆盖绕过 policy。
- UID、DTSTAMP、SEQUENCE、时间/结束/时长、STATUS、CLASS、TRANSP、重复规则/日期/取消、所需 VTIMEZONE 是保留的协议与时间语义。这不是匿名化或隐藏事件存在/时段的能力。允许的正文可能自行重复隐藏字段的文字；不做语义内容识别。需要完全隐藏正文时用空字段白名单。
- 原始文档先经过既有 renderer 检查，再投影/渲染，不能通过清除字段使原先不支持的文档变为成功。#31 目前拒绝 override/ranged exceptions、people、非空 alarms/attachments/extensions 等；本项保持该支持边界。override 清除作为 policy 的单独回归验证，不宣称 HTTP 已支持 override。

`Storage::issue_scoped_feed_token` 在同一事务内验证 policy/归属并签发受限 token，不先暴露临时的整来源授权。`Storage::configure_feed_scope` 在事务内检查归属并按预期配置 revision 做 CAS；撤销记录不能借配置修改复活。`authorize_feed` 返回包括已验证 scope 的 `FeedAuthorization`。历史 `authenticate_feed_token` 仍只接受字面 `{}`、仅返回 source_id，继续拒绝受限 policy，防止旧调用方误把受限能力当作整来源授权。

## 快照、缓存与并发

授权、scope、calendar 归属、source revision、事件和最新事件修订时间在 SQLite 一致读快照中取得。事件按内部 ID 排序，包含保留文档的删除墓碑；不依赖提醒任务或规划窗口。缺失事件修订元数据使整份请求失败，不静默遗漏。每份快照最多 10000 事件、16 MiB 原始 JSON；输出受服务 max_response_bytes 限制，不截断返回部分日历。

权限检查先于每次缓存查找及 If-None-Match 比较。缓存键包括订阅 ID、配置 revision、source ID、数据 revision 和原始 scope 的 SHA-256；ETag 再绑定完整键及最终输出字节。不同配置或数据 revision 即使生成相同正文也有不同 ETag。缓存最多保留 16 份受输出预算约束的文档，满后清空；无共享缓存或长期授权缓存。

渲染/缓存读取后、返回 200/HEAD/304 前再次查询持久化授权并比较完整键。等待期间撤销、禁用、到期、轮换导致拒绝；policy 或数据修订变化返回 503，客户端可重新拉取。最后一次检查之后仍存在无法与 socket 写入原子化的变更窗口；请求授权以最后检查时刻为准，不承诺撤回已经传输或客户端已保存的内容。

成功/304 返回 `Content-Type: text/calendar; charset=utf-8`、`Cache-Control: private, no-cache` 和 ETag；支持强/弱 If-None-Match、列表和 `*`，304 无正文。拒绝返回固定错误 envelope，不带旧 ETag 或事件正文；已匹配 feed 路由的错误标记 `private, no-store`。不存在、错误、过期、撤销、未知 scope 都返回 404，存储/预算故障为 503，无法渲染为 422。通用服务边界仍负责请求标识、超时与响应预算。

迁移 `0008_feed_configuration_revision.sql` 只添加触发器：public_id/source_id/scope_json/expires_at 实际改变且调用方未递增 revision 时，自动递增配置 revision；显式 CAS 或轮换不重复计数。无新权限列、不修改旧迁移或 checksum。集成回归从已应用 0009 且未应用 0008 的真实历史升级，检查旧备份和升级后备份均可恢复；未知或改变 checksum 的历史仍由既有备份校验拒绝。

## 日志与部署边界

服务不记录请求 URI、query、public_id、token、ETag、ICS 正文或 renderer 的含内容诊断；HTTP 错误仅返回固定 code 与服务生成的 request_id。feed 诊断用固定类别。真实子进程回归从 loopback 发送成功 GET/HEAD、405、404 及非法 UTF-8 路径，检查 stdout/stderr 和错误响应均不含 capability/完整路径。

这是服务进程边界。反向代理、Cloudflare、浏览器、日历客户端或上游基础设施不由本代码自动改配置；部署时必须关闭 feed 原始访问日志，或只记录固定 `/feeds/[REDACTED]` 模板、method/status/request_id，禁止 `$request`、`$request_uri`、query 和 Referer 等可能含 capability 的字段。不要把订阅 URL 当作普通可公开链接。

## AC 与验证

| AC | 验证 |
| --- | --- |
| AC1 | storage/feed：摘要、用途隔离、到期等号、撤销回跳不复活、重开、并发轮换唯一赢家；server/feed：缓存填充后撤销/禁用/过期/轮换与旧 ETag 均拒绝 |
| AC2 | HTTP：200/HEAD/304、private no-cache、稳定 ETag、配置/数据 revision 分别失效；prepared response fence：独立任务提交 policy/data/expiry/rotation/revoke/disable 后拒绝旧表示 |
| AC3 | application/feed_contract：逐字段及 override 独立投影、严格 schema 负例；storage：跨 source 拒绝、calendar 过滤、一致旧/新快照、缺 metadata 故障；HTTP：缩权后旧 ETag 得到新正文或空日历、renderer 边界；feed_process：真实日志无 URL/token |
| 兼容/迁移 | 已应用 0009 后补 0008，升级前后备份/恢复，旧 `{}` 行为及 source-only helper 不扩大授权 |

有效 Red：首次两个 HTTP 测试观察到 404 而预期 200；scope 接入前修正为 renderer 支持的取消例外后观察到 v1 请求 404 而预期 200。超出 renderer 支持边界的 override 夹具 422 及编译/工具错误不计 Red。严格 schema 回归检出了 serde 将 `[]` 接受为空 struct 的问题，现显式要求 JSON 对象。受控移除最终 fence 的完整键比较后，回归实际得到 304 而预期 503（scope 变更），确认测试能检出权限缩小后的错误 304；随后恢复正确实现。最终完整验证和审核结果见 PR。

参考：通过 Exa 查阅 [Axum Router merge/state 官方源码](https://github.com/tokio-rs/axum/blob/axum-v0.8.6/axum/src/routing/mod.rs)，采用独立 router/state 后最小 merge；条件请求采用 [RFC 9110 §13.1.2](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.2)的弱比较，缓存指令依据 [RFC 9111 §5.2](https://www.rfc-editor.org/rfc/rfc9111.html#section-5.2)；协议编码仍采用[既有 renderer](ics-rendering.md)，不重新实现 RFC 5545。
