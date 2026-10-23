# 任务租约、重试与恢复（#24）

`calendar-application::deliver_one` 执行一次有界扫描/投递；`DeliveryRepository` 由 SQLite `Storage` 实现，`PushOutput` 由输出适配器实现。服务生命周期与唤醒装配留给 #27，Bark 留给 #33；当前测试仅使用模拟输出。

## 事务与状态

1. `claim` 使用 `BEGIN IMMEDIATE`，先回收最多 64 个过期租约，再扫描最多 64 个到期候选。暂停的规划/时区/陈旧来源在 SQL 中筛掉，避免挡住其他来源。领取条件 UPDATE、随机 owner/token 租约和 `delivery_attempts` 记录同事务提交。
2. `prepare` 复核 owner/token/未过期租约、任务状态、最新 generation、事件/来源/规则/输出、日历选择、实例取消、解释时区、tzdata、规划水位与来源新鲜度。旧修订作废、取消不发送、陈旧或待规划任务暂停；截止时间等号即过期；在领取后的 UTC 后跳中，再次检查 due_at 与 next_attempt_at，尚未到期记 not_sent 并延期。记录 `sent_at` 与随机 `request_id`，重新延长租约后释放事务。重复 prepare 被拒绝，即使首次 prepare 后发生取消，也保留原在途结果。
3. 事务外执行输出，Tokio 单调计时器约束请求总超时；超时分类为 `unknown`。适配器必须能被取消且不能阻塞执行器。默认请求超时 30 秒，租约 60 秒；配置强制租约长于超时，不支持无界长请求。
4. `finish` 在短事务中再次检查 owner/token 和租约，原子写入 attempt 结果、job 状态及逻辑 `accepted_at`。过期/旧 token 回写被拒绝。取消或 superseded 的在途请求仍记录真实 outcome，不复活旧 job；真实 accepted 保留逻辑去重并作废同一逻辑身份的新任务。

取消无法与外部发送原子化，最终检查后仍可能收到一次旧提醒。已记录发送意图但崩溃时，恢复只能判断结果未知；重试可能重复。`accepted` 仅指输出服务接受，不表示设备已显示，也不提供 exactly-once。

## 时间、重试与审计

所有持久化时间使用 UTC Unix 毫秒，调度/租约通过可注入 `Clock` 重新扫描；请求超时使用 Tokio 单调时钟。时钟后跳不会提前发送未来任务，前跳超过截止时间直接过期。UTC 后跳可能延后租约回收；不把内存计时器作为任务真相源。

`deadline_at` 在首次领取时持久化为 `due_at + catch_up_ms`，默认追赶窗口 15 分钟；已有截止时间不延长。默认最多 8 次领取尝试，指数退避采用 token 派生的 equal jitter，默认基数 1 秒、上限 60 秒。非负 Retry-After 是最早重试的下界；下一次时间达到截止时间即 `expired`，次数耗尽为 `failed_permanent`。包括领取后未发送的崩溃在内，尝试都会计数，避免无限循环。

默认来源新鲜度为 24 小时，`last_verified_at` 缺失或过旧暂停发送。调用者须提供与规划器一致的 `tzdata_version` 和同一服务级策略。持久化任务的截止时间不因后续策略调整重新计算。

迁移 `0004` 增加 `delivery_attempts`：唯一 `(job_id, attempt_no)`、owner/token、request_id、领取/发送/完成时间、分类 outcome、最终 disposition、Retry-After。只保存分类，不保存自由格式错误、凭据或事件正文。`sent_at` 是发送意图，不证明已发出；租约到期时有意图记 `unknown`，无意图记 `not_sent`。已 accepted 的历史任务不伪造历史 attempt。

数据库 busy 返回可重试的 `DeliveryError::Busy`；其他存储错误脱敏。一次失败不会提交部分领取或 accepted 状态。恢复下一轮扫描由调用者负责，不在数据库忙时紧密重试。

## AC 与实际验证

| AC | 验证 |
| --- | --- |
| AC1 | 多连接竞争只有一个 owner；错误/旧 token、重复 prepare/finish、租约到期边界拒绝 |
| AC2 | Retry-After 下界、重试上限、截止、取消、修订变化、待规划、时区版本、65 个暂停任务不饿死 ready 任务 |
| AC3 | 领取与 accepted 的 SQLite trigger 故障回滚；重开保持 job/attempt/accepted 一致；取消竞争和新 generation 去重 |
| AC4 | 模拟输出期间真实 SQLite 写入；输出超时；真实子进程 prepare 后强制终止并恢复；短 busy timeout；UTC 前后跳（包含 claim 与 prepare 之间后跳及 Retry-After 下界） |
| 迁移 | 合成 v1/v2/v3 → latest、v3 冲突迁移失败保留历史、重开和外键检查 |

有效 Red：领取后 UTC 后跳曾提前发送，回归先失败再修复；发送后取消再重复 prepare 曾将结果误记为 not_sent，回归先失败再修复；在原实现中测试因缺少持久化 attempt 表失败；初版扫描在 65 个暂停任务前返回空，回归在修复后通过。受控移除租约到期校验后，旧 token 回写回归产生断言失败；恢复生产实现后通过。同步→规划→模拟投递→重规划的集成验证确认真实逻辑去重。迁移与并发测试使用真实文件库；进程测试通过准备完成文件协调，10 秒仅作为测试失败上限。未运行形式证明、真实网络投递或服务进程的生命周期装配。

运行：`cargo test -p calendar-storage --test delivery`；迁移：`cargo test -p calendar-storage --test foundation`；完整检查遵守 [贡献指南](../CONTRIBUTING.md)。

## 实现参考

通过 Context7 `/websites/rs_sqlx` 查询事务、`begin_with` 和 `busy_timeout`，再核对本地锁定 SQLx 0.8.6 公共源码的 `Pool::begin_with`、`Transaction::drop`、SQLite transaction worker 和扩展错误码。检索返回 latest 文档，因此以锁定源码与实际集成测试核对 API。

- [SQLx 0.8.6 SqlitePool](https://docs.rs/sqlx/0.8.6/sqlx/type.SqlitePool.html)：自定义 `BEGIN IMMEDIATE`。
- [事务及公共实现](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/transaction.rs)：未 commit 的事务 Drop 回滚。
- [SQLite 连接选项](https://docs.rs/sqlx/0.8.6/sqlx/sqlite/struct.SqliteConnectOptions.html)：busy timeout。
- [SQLite SQLx 错误实现](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/error.rs)：扩展错误码低八位用于 busy/locked 分类。

通过 Context7 `/websites/rs_tokio_tokio` 核对 [Tokio timeout](https://docs.rs/tokio/1.54.1/tokio/time/fn.timeout.html)：超时取消 Future，需要启用 timer 的 Tokio runtime，不能抢占不 yield 的代码；对应限制已写入 `PushOutput` 用例契约。
