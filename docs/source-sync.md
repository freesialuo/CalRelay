# 来源同步事务（Issue #22）

用户故事：作为来源管理员，希望一次提交完整可信的来源变化，使并发、失败或重试不会丢事件或推进半成品游标。

## 契约与设计

`calendar-application` 定义 `SyncBatch`、`SyncMode`、`SyncRecord`、删除策略及 `EventRepository`；只依赖 domain，不依赖 SQLx。`calendar-storage` 实现一次批次的短事务。

1. 写事务外检查批次形状和领域规则，拒绝空来源/UID/日历、负版本、重复 UID、超过 10,000 条的批次及不完整快照；序列化完整 EventV1 并计算 SHA-256。完整 JSON Schema 校验仍由输入边界执行。
2. `BEGIN IMMEDIATE` 先取得 SQLite 写事务，再检查来源 enabled 与 base_revision；并发写者串行比较最新版本，不持有旧读快照后升级写锁。
3. Delta 仅删除显式 UID；完整快照只接受 upsert，按来源内仍存活的缺失 UID 删除。实际删除数超过 `DeletionPolicy::Limit` 即整批拒绝；`Approved` 只允许已取得管理员批准的可信调用方使用，不是面向外部请求的权限开关。
4. 同源 UID 保持内部 ID。正文摘要、日历及删除状态均相同时不写事件修订；日历移动、内容变化或恢复墓碑则产生新修订。显式删除未知/已删除 UID 是无事件变化操作。取消状态作为普通 EventV1 更新保留，不等同于删除。
5. 保存当前正文、事件修订及来源变化日志，最后以条件 UPDATE 原子推进来源版本、checkpoint 和核验时间；任何失败整体回滚。每个成功批次均推进来源 CAS 版本，包括空增量和重复内容，避免旧结果覆盖新游标；无事件变化时不产生 `event_revisions`/`source_changes`。304 不通过此接口提交，由 [#29 协调器](source-coordinator.md) 单独处理。

事件修订从 1 开始；时间为调用方注入的 UTC Unix 毫秒。revision 溢出返回错误，不回绕。两份文档均有 `source_sequence` 时拒绝较小的新值；缺少序列时依赖来源 CAS。墓碑保留当前正文与摘要，删除日志正文为 null，恢复沿用 ID 并递增修订。未支持的规划语义继续由能力门槛处理，本接口保存经过领域校验的数据不代表可执行所有日历能力。

输入遍历及快照成员检查使用 HashSet，避免逐条扫描输入；SQL 查询由已有源内 UID 和来源变化索引支持。未测量吞吐量、分配次数或尾延迟，不作性能提升结论。批次条数上限不替代未来请求大小/解析预算；快照扫描的内存取决于该来源已有事件数。

## V-Model 对应

| 验收条件 | 设计 | 可执行测试 |
| --- | --- | --- |
| AC1 批次与输入不变量 | 纯校验在锁外 | application `sync_contract` 两项测试；storage `invalid_or_incomplete_batches_never_write_or_advance_checkpoint` |
| AC2 原子提交和无变化去重 | 一个事务覆盖事件/两类日志/来源状态 | `commits_events_history_checkpoint_and_reopens_consistently`；`unchanged_sync_advances_cas_without_duplicate_event_revisions` |
| AC3 隔离、CAS 和序列 | 先写锁、来源约束、检查单调值 | `concurrent_same_revision_has_exactly_one_winner`；`calendar_scope_disabled_unknown_source_and_revision_overflow_are_rejected`；`older_upstream_sequence_cannot_overwrite_newer_content` |
| AC4 完整性与删除审核 | 模式类型及来源内缺失集合 | `full_snapshot_deletes_only_missing_live_events_and_requires_approval`；`identical_uids_in_other_sources_are_untouched_by_snapshot_deletion`；不完整批次负例 |
| AC5 身份和恢复 | 稳定 ID、墓碑及整体回滚 | `delta_delete_restore_and_calendar_move_preserve_internal_identity`；`database_failure_rolls_back_earlier_writes_and_the_source_cas` |
| AC6 工程过程 | 用户故事、准入和验证映射 | [开发方法](development-method.md)、Issue 模板及 Rust/契约检查 |

测试路径：`crates/calendar-application/tests/sync_contract.rs`、`crates/calendar-storage/tests/sync.rs`。故障测试用 SQLite trigger 在已有写入后中止日志插入，再重开文件核对旧事件、日志和游标；无需向生产代码注入失败开关。它不等同于进程强杀/掉电演练，后者在 #24/#37 完成。

## 实际 TDD 记录

- Red：先定义仓储入口与 9 项同步测试，事务入口尚未实现。运行 `cargo test -p calendar-storage --test sync`，1 通过、8 失败；失败包括提交返回 Conflict、并发没有成功者、跨源日历错误类别不符。
- Green：实现事务后，发现最大 revision 场景的测试夹具在构造核验时间时先溢出；修正夹具为饱和加法，该夹具错误不计作产品缺陷或需求红灯。
- Refactor/回归：补充跨来源相同 UID 的快照隔离测试及两项纯契约测试；验证序列化/摘要在锁外和批次上限，运行完整工作区检查。

范围：不提供 HTTP、分页暂存、来源创建 API、规划任务或真实发送。来源/日历由后续受保护管理路径配置，仓储只接受可信上下文中的 source_id。版本不匹配返回 Conflict，调用方应重新获取状态，不盲目以新版本重放旧批次。
