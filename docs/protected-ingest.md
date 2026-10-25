# 来源作用域机器写入（#30）

`PUT|DELETE /api/ingest/v1/sources/{source_id}/events/{uid}` 使用独立 API bearer token。
这细化了架构文档的路由示意：来源显式在路径中，并必须等于 token 的绑定来源。
PUT 正文为 `{ "calendar_id": "…", "event": EventV1 }`；拒绝未知字段，event.external_uid 必须与解码后的路径 UID 完全一致。
正文不能指定 source_id。calendar 必须属于同一来源。DELETE 正文必须为空。
不提供管理登录、feed、外部格式解析、生产任务或真实推送。

## 条件、重放与错误

两个动作都要求 `Authorization: Bearer …` 和 `Idempotency-Key`（1–128 个 ASCII graphic 字符）。
必须恰好一个条件头：初次 PUT 用 `If-None-Match: *`；修改、删除及恢复 tombstone 用 `If-Match: "N"`，N 为当前事件正整数 revision。
不接受弱 ETag、列表、通配 If-Match、重复条件头、双条件或缺失条件。
不存在事件的 DELETE 返回 409；tombstone 保留身份，不能用初次写条件复活。
删除已删除事件若 revision 匹配则不再新增修订；相同内容、日历的 PUT 也不新增修订。
事件内容改变/恢复/删除同时推进事件和来源 revision，写 event_revisions/source_changes，保留远程 checkpoint。
source_sequence 递减返回 409，与既有同步规则一致。

创建返回 201，其他成功返回 200；ETag 是带双引号的事件 revision。JSON 包含 event_revision、source_revision、deleted、request_id、idempotency_expires_at（UTC Unix 毫秒）。
重放返回首次成功的相同状态、正文、ETag，包括首次 request_id；每次 HTTP 尝试的 X-Request-Id 由公共边界重新生成。
错误沿用公共 `{error:{code,request_id}}`，并与本次 X-Request-Id 一致；401 附 WWW-Authenticate。
Schema/领域/UID/calendar/条件非法返回 422；CAS、幂等摘要、上游序列冲突返回 409；持久化错误 500；停止或容量耗尽 503。
错误不回传事件正文、token、幂等键或 SQL 异常。成功响应 `Cache-Control: no-store`。

幂等键命名空间为 token 身份（token-wide），不是每条 UID：同键换路径、动作、条件、日历或事件返回 409。
摘要是带版本的结构化元组（source、UID、动作、条件、日历、类型化序列化后的事件）；忽略输入 JSON 的空白和对象字段顺序。
显式缺省/null 及领域反序列化的等价形式按类型化结果处理，不保证原始字节级摘要。
仅成功提交保存 receipt；无效、权限拒绝和冲突不占用新键。不会延长重放保留期。
权限复核先于重放，撤销、过期、禁用、动作收窄立即阻止之后的请求；不会通过旧 receipt 绕过。

## 事务、安全与容量

唯一 `BEGIN IMMEDIATE` 取得写锁后采样 Clock，复核 token 的 digest、source、action、revoked_at、expires_at 和 source.enabled；随后同事务读取 receipt、检查 CAS、写事件/日志/来源及 receipt。
不使用先 authenticate_api_token 再独立 commit_sync 的两段提交。
撤销和其他配置写入由 SQLite 写锁串行化：先提交的撤销必须阻止后续写入；已取得写事务的写入可先于等待中的撤销完成。
到期检查按取得锁后的事务时间，到期等号失效；不是跨进程时钟/管理员回滚防护。
SQLx 事务 drop 回滚，receipt 落盘失败不能留下事件改变。HTTP 超时/响应丢失时提交结果可能未知，使用原键重试核实。

服务初始化读取并验证以下配置，越界/非数字启动失败：

| 环境变量 | 默认 | 有效范围 |
| --- | --- | --- |
| CALRELAY_INGEST_RETENTION_MS | 86400000（24 小时） | 60000–604800000（1 分钟–7 天） |
| CALRELAY_INGEST_MAX_RECORDS | 100000 | 1–1000000（全库 receipt） |

过期等号起允许键重用，并重新执行当前 CAS；因此过期的初次创建重试通常返回 409，不再返回旧 201。
不承诺永久幂等；客户端必须记录截止时间，超过期限按当前状态恢复流程。
清理只删除 expires_at<=事务时间的记录；容量不足拒绝新写入，绝不淘汰未过期 receipt；已存在的有效重放仍可成功。
UTC 回跳不会提前清理旧 receipt；时钟向前跳变可能提前到期，运维须保持可信 UTC。
索引按过期时间清理；条数有上限，键/摘要只存固定 SHA-256、版本/状态/截止时间/请求 ID，不存原始键、bearer 或事件副本。
不自动 VACUUM，不承诺有界数据库文件字节数；事件历史本身仍按既有契约保留。

## 迁移与并行集成

0007_ingest_idempotency.sql 新增独立 receipt 表与过期索引，不修改旧迁移或 checksum。
基线 main c57468b 已包含 0009；真实旧 schema 1–6,9 能补入 7，备份校验按已知版本/checksum 接受非连续历史，旧备份恢复也可升级。
新 receipt 随备份保存，恢复仍默认暂停投递，重放不会改变暂停策略。
备份可回滚 token 撤销，恢复库须继续按既有隔离流程核实；receipt 不是抗恶意数据库回滚日志。
共享生产改动仅 application/storage 模块导出、token digest 的 crate 内复用，以及 server 模块/Services policy/HTTP router/OpenAPI 的最小追加。
0008 feed、0010 管理认证未纳入本分支；父任务仍需在实际合入后组合验证。
无 Cargo.toml/lock 改动或依赖升级。

## AC 与验证证据

| AC | 测试 | 层级 |
| --- | --- | --- |
| AC1 | authorization_validation_and_revocation_before_replay；expired_disabled_action_and_malformed_conditions_fail_closed | HTTP + 真实 SQLite 安全负例 |
| AC2 | conditional_retry_delete_and_tombstone_contract；concurrent_cas_has_one_winner_and_same_key_replays | HTTP 条件/幂等/并发 |
| AC3 | HTTP helper 逐次断言 422/409 和错误 request_id；领域 end<start、未知字段、calendar 越权、条件头负例 | 契约/集成 |
| AC4 | revocation_and_expiry_linearize_before_waiting_writer；retention_capacity_and_receipt_failure_are_atomic | 实际锁竞争、假时钟、故障回滚 |
| 兼容 | v9_without_v7_upgrades_and_backup_restore_preserves_receipts | 历史迁移、备份、恢复、receipt |

已有 sync 10 项基线通过。有效受控 Red：去掉事务内 revoked_at 谓词后，HTTP 撤销重放测试实际返回 201（预期 401）失败；恢复实现后回归 Green。
此为测试灵敏度见证，不把编译、依赖下载或夹具问题算需求 Red；未声称初始全部实现前完成 TDD。
无真实服务/密钥调用，无长时 fuzz、形式证明或生产掉电验证。

实现采用 [SQLite 官方事务说明](https://www.sqlite.org/lang_transaction.html)：IMMEDIATE 先取得写事务，SQLite 只允许一个同时写入者；WAL 仍需短事务及 busy timeout。

本轮实际本地 gate：cargo test --workspace --all-features --locked 共 229 项通过（新增 7 项），专项 HTTP 最终 4 项通过；fmt、workspace/all-targets/all-features Clippy -D warnings、Python unittest 17 项、check_repository.py、git diff --check 通过。requirements/ci.txt 在既有 .venv 中确认满足；Cargo 1.99.1 使用保存环境既有工具，不重装或更新锁文件。远端 CI/review 以 PR 当前 head 为准。
