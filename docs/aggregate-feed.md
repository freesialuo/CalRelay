# 托管多来源汇总订阅后端

[#100](https://github.com/freesialuo/CalRelay/issues/100) 实现 [#76](https://github.com/freesialuo/CalRelay/issues/76) 的存储、管理和只读 ICS 后端。首版前端创建/编辑与来源选择、真实客户端系统验收仍由 #76 跟踪，本页不宣称这些已交付。

配置使用独立的数字 `version: 1`，与既有 feed scope 格式一致；来源配置自身的字符串 `v1` 格式保持不变。完整结构见 [schema](../schemas/aggregate-subscription-v1.schema.json) 和 [禁用创建示例](../examples/aggregate-subscription-v1.json)。所有字段必填，`expires_at` 为 UTC Unix 毫秒或显式 null。最多 32 个托管来源、所有来源合计 256 个日历；ID 必须是 1–128 个 ASCII 字母、数字、下划线或连字符。名称最多 256 UTF-8 字节且非空白、不含控制字符。重复来源、重复日历、跨来源日历、未选中日历、未知版本/字段/规则全部拒绝；schema 描述结构，Rust 校验总量与数据库归属。

`sources: []` 是合法空日历；成员的 `calendar_ids: []` 也不授权任何日历，但仍检查这个成员的存活与新鲜度。`visible_fields` 仅允许 summary、description、location、url、categories。空数组保留协议必要内容，summary 使用固定占位；覆盖实例递归应用相同投影。UID、时间、状态、重复规则及取消实例属于协议必要信息，仍然可见。复用[单源 feed](readonly-feed.md) 的既有 renderer 和投影，不扩大其可支持语义。不同来源同一外部 UID 不去重；输出身份仍由全局持久 event_id 生成，来源或订阅改名不会改变身份。

管理路由复用现有 Access 身份、管理会话、Origin、CSRF 与单次写 nonce。GET/list 不返回 token；PUT 创建要求 `If-None-Match: *` 且 enabled=false，返回 201 与一次性 `capability_url`。此 URL 是相对路径，前端应基于可信公开服务 origin 解析，不依据不可信 Host 构造；展示后不持久化、不发送到遥测。普通 PUT 编辑要求 `If-Match: "revision"`，返回记录且保留原链接。POST `/api/admin/v1/subscriptions/{id}/token` 要求当前 revision 和空 body，显式轮换并一次性返回新 URL。DELETE 要求相同 CAS 与空 body，永久退休，不能重新激活。业务失败也可能消费 nonce，客户端须读取 `X-Admin-Write-Nonce`。配置编辑、轮换、退休均递增订阅 revision；审计仅保存动作、revision、时间和 actor 摘要。

公开路由为 `/feeds/aggregates/{public_id}/{secret}.ics`，不要求管理登录或 Turnstile。存储只保存独立 aggregate-feed purpose 的 token 摘要；API 写 token 与旧单源 token 不能替代。既有 `/feeds/{public_id}/{secret}.ics`、legacy `{}` scope、旧 token 与 expiry 不变。订阅链接是秘密，不记录完整 URL/token、请求路径/查询/头或 ICS 内容；现有请求日志仅包含固定阶段/请求 ID。反向代理也须关闭原始 capability URL 访问日志。

每次读取先在同一个 SQLite 读事务中验证订阅与全部来源，随后查询缓存；未命中时在一个事务内读取授权向量和所有事件。缓存键包含订阅 revision/配置摘要及每个来源的配置 revision/摘要、数据 revision、显式日历选择与新鲜度证明。渲染后再次验证完整向量；变动返回失败，绝不以旧 ETag 返回 304 或旧 payload。授权成功时支持 ETag/304 与 `Cache-Control: private, no-cache`，失败为 private no-store；内存缓存最多 16 份并受每份输出上限约束。

首版 `stale_policy` 只接受 reject：任何成员禁用、退休、失权或未知配置均整份拒绝；新鲜度不满足返回不可用，不悄悄删掉来源。Pull 来源还须有已提交 snapshot revision 证明；有效期按现有来源 SLA（含截止毫秒）验证，未来验证时间拒绝。Manual-static 不强加拉取 SLA，push-only 仅在配置显式 SLA 时要求新鲜度。配置 owner 的已有来源存储/读取 port 是唯一来源契约，不修改提醒规则、Bark output 或凭据初始化。

预算是全部来源合计：10,000 个存储事件（含 tombstone）、16 MiB 原始文档、服务配置的单份输出上限。查询先统计总输入，再读取；renderer 在投影前累计原始事件与 recurrence exceptions 的保守编码预算，最后复核实际字节。超限整份失败，不能逐来源乘大或截断。未支持的事件依然拒绝，不能靠隐藏字段掩盖；清理旧 tombstone 不在此范围。

迁移 `0013_aggregate_subscriptions.sql` 只新增独立表，不重建旧 feed、安全或来源表。测试覆盖真实 0012 已应用数据库升级、旧/新备份 checksum/version 校验及恢复后的新旧 capability、并发旋转 CAS、一致快照、来源失权、expiry、最终缓存 fence 和合计输入预算。HTTP 测试通过真实管理 guard 与本地 SQLite，全部身份、事件和来源均为合成数据；没有访问真实外部日历。

| 叶子 AC | 风险与实际验证 |
| --- | --- |
| AC1 | storage/aggregate：摘要与用途隔离、并发轮换唯一赢家、重开与退休；注入审计失败后配置/成员/token/退休全部回滚；0012→0013 旧/新备份与 capability 恢复 |
| AC2 | application/aggregate_contract 与 Python：有限显式策略、未知字段拒绝、总日历预算；storage/aggregate：两源同 UID 独立身份、未提交旧快照/提交后新快照、合计事件/输入预算、新鲜度及失权负例 |
| AC3 | server/aggregate_feed：渲染后的并发配置/数据/选择/expiry/rotation/retirement/disable fence；server/aggregate：暖缓存 304、字段缩小、总输出预算（隐藏 summary 仍收费）、稳定 UID；feed_process：真实进程日志与错误响应无新旧 URL/token |
| AC4 | server/aggregate：真实 guard 登录、CSRF 拒绝、创建/编辑/CAS/轮换/退休、非秘密发现；生成 OpenAPI 与前端现有 21 个 Chromium 回归。聚合 UI 和真实客户端互操作仍待 #76 |

有效 TDD Red：只读 port 尚未实现时，真实 SQLite 双来源快照测试返回 Denied，而期望包含两个事件；实现一致授权/快照后通过。UID 明文假设夹具错误、编译和 OpenAPI 测试夹具预算错误不计需求 Red。完整命令、最终 head 和远端检查以 PR 为准。
