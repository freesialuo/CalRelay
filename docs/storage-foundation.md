# SQLite 存储基础：V-Model 与 TDD

范围：[Issue #21](https://github.com/freesialuo/CalRelay/issues/21)，父项 [#4](https://github.com/freesialuo/CalRelay/issues/4)。`calendar-storage` 仅提供文件连接、版本迁移及五张事件核心表；同步事务、规划和投递在后续 Issue 交付。

## 需求、设计与验证

需求分析先确定持久化、隔离和失败保护的可观测结果，再将系统设计分配到连接池、迁移与关系约束，按对应层级验证。测试均使用临时文件数据库，不以仅内存测试代替重开验证。

| 需求 | 设计约束 | 对应验证（`tests/foundation.rs`） | 验证层级 |
| --- | --- | --- | --- |
| R1：稳定、有限的持久连接 | 最多 4 条；每条连接 WAL、外键 ON、FULL、busy timeout 5000ms | `every_connection_has_durable_pragmas_and_the_pool_is_bounded` 同时持有四条连接，逐条查询 PRAGMA 并尝试孤儿写入 | 系统/集成 |
| R2：初始化可重入，提交后重开不丢数据 | 内嵌迁移，校验历史版本和 checksum | `migrations_are_repeatable_and_committed_data_survives_reopen`：建库、写入、关闭、两次重开、完整性及外键检查 | 验收 |
| R3：来源隔离且日志可追溯 | 来源内 UID 唯一；事件/日历、来源/事件、变化/事件修订复合外键 | `event_identity_is_source_scoped_and_calendar_ownership_is_enforced`；`revision_log_rejects_cross_source_and_missing_event_revisions` | 集成 |
| R4：数据库拒绝局部非法状态 | STRICT 类型、有效 JSON、非负修订、合法 enabled/change_kind，upsert 日志必须有正文 | `invalid_json_revisions_and_change_kinds_are_rejected`：逐条检查不变量，也验证合法删除日志 | 规则级，通过真实数据库验证 |
| R5：失败不伪装成功或销毁旧数据 | 迁移不匹配/未完成即返回错误并关闭池；不删库、不自动修复 | `incompatible_migration_history_fails_without_destroying_data`；`invalid_paths_and_corrupt_files_are_reported_without_replacement` | 验收 |
| R5：未提交写入不得落入后续视图 | SQLite 事务回滚 | `uncommitted_writes_do_not_survive_reopen` | 集成 |

上述规则验证覆盖本阶段的细粒度设计；无需再以 mock 复制同一 SQL 实现。真实进程强制终止、磁盘满、DB busy 竞争及任务恢复在后续 [#24](https://github.com/freesialuo/CalRelay/issues/24)、[#37](https://github.com/freesialuo/CalRelay/issues/37) 验证，本阶段关闭重开和回滚测试不代表这些演练已完成。

## TDD 记录

1. Red：先编写 8 项文件数据库测试，连接入口只做默认连接，未配置连接池策略或迁移。运行 `cargo test -p calendar-storage --test foundation --locked`：0 通过、8 失败。实际失败包括最大连接数 10 而非 4、`no such table: sources`、损坏文件被接受。
2. Green：增加显式连接选项、内嵌迁移及关系约束，同一组 8 项测试全部通过。
3. Refactor：保持连接与迁移入口简单，补齐来源变化类型一致性、upsert 正文必需及未完成迁移历史的边界案例；运行工作区完整检查。

重复执行：`cargo test -p calendar-storage --test foundation --locked`。首轮 Red 的最小连接入口不保留在生产代码中；测试始终通过公开 `Storage::open` 入口检查实际行为。

## 存储契约

- `Storage::open(path)` 在已有目录中建库/打开并完成迁移后才返回；调用者需在完成后宣布 ready。失败返回 `OpenError::Database` 或 `OpenError::Migration`。
- `pool()` 仅供存储仓储实现使用；domain/application 不依赖 SQLx。`close().await` 等待已借出连接归还后关闭池，关闭前不应删除数据库目录。
- `0001_event_core.sql` 建立 `sources`、`calendars`、`events`、`event_revisions`、`source_changes`；`_sqlx_migrations` 由 SQLx 管理。迁移内嵌二进制；build script 追踪 migrations 目录。已应用迁移不修改，后续变化新增版本。
- SQLx 固定 0.8.6，使用其 bundled SQLite。STRICT 表要求 SQLite ≥ 3.37；仅按本地单实例 WAL 设计使用。连接检查结果不等于掉电或硬件持久性证明。
- 时间戳存 UTC Unix 毫秒；`revision` 从 0 开始。`committed_checkpoint` 是可缺省的来源游标字符串，`config_json` 和 `timezone_policy` 是 JSON 配置。凭据及 credential 外键留待安全存储迁移。
- `events.document_json` 是当前规范化事件正文，数据库只检查 JSON 合法性；完整结构和跨字段语义仍由 schema/领域校验负责。测试中的 `{}` 用于隔离数据库约束，不作为合法 EventV1 样例。
- 删除保留事件行及可选 `deleted_at`；历史 `delete` 可以省略正文，`upsert` 必须携带正文。来源变化的类型必须与指向的事件修订一致。硬删除不会级联抹去历史。
- 索引覆盖源内 UID、日历事件和事件修订查找，`source_changes` 主键前缀支持按来源 revision 读取。事件与 checkpoint 的原子提交由 [#22](https://github.com/freesialuo/CalRelay/issues/22) 实现，当前不宣称已有同步仓储接口。

文档依据见 [项目参考](references.md#sqlite-存储实现)。
