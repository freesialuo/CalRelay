# 提醒规划与持久化水位（#23）

Issue：[持久化实例与提醒规划水位](https://github.com/freesialuo/CalRelay/issues/23)，里程碑 M1。
前置 #19、#20、#22 已关闭，对应实现均在分支基线 `7c124eb` 中。
优先选择此项，因为它解除 #24 的调度依赖，随后支持 #33 的 Bark 投递。

## 范围与判定依据

- 复用 [领域能力](domain-capabilities.md) 的受限展开档位，不重新实现 RRULE。
- 遵守 [架构](architecture.md) 的规划事务及 [事件模型](event-model.md) 的身份、generation、accepted 去重约束。
- 验证应用规划和真实 SQLite 持久化；队列领取、HTTP 投递及租约恢复属于 #24/#33。
- 首版仅服务端单次相对开始偏移和全天本地提醒时间；不执行导入 alarms。
- 固定 UTC 时钟与单调预算时钟分别注入；测试不依赖宿主机时区、真实时间等待或外部服务。

## AC → 测试场景

| AC / 编号 | Given / When | Then：可观察结果 |
| --- | --- | --- |
| AC1 P01 | UTC 单次事件 `2026-11-02T17:00:00Z`，提前 15 分钟 | 恰一实例和任务；`original_key=single`，触发 `16:45Z`；起止及事件修订准确 |
| AC1 P02 | 全天事件 `2026-11-02`，日历 `America/Vancouver`，本地提醒 09:00 | 触发 `17:00Z`，保留本地日期；全天跨度沿用领域语义，不硬编码 86400 秒 |
| AC1 P03 | floating `2026-11-02T09:00:00`，显式温哥华日历时区 | 解析为 `17:00Z`；缺失/未知时区明确失败；UTC/zoned 不受无关日历时区影响 |
| AC1 P04 | 采用已评估 DAILY/WEEKLY/MONTHLY/YEARLY、月末、闰日、EXDATE、PERIOD RDATE、改期/取消样例 | 固定预期 original_key、UTC 起止、任务触发时间；取消实例不产生可发送任务；不以调用同一展开函数生成 oracle |
| AC1 P05 | 来源自定义 VTIMEZONE（含与 IANA 同名）、THISANDFUTURE 或未支持 RRULE | 返回明确诊断，不回退同名 IANA，不静默截断；不推进成功水位或部分发布任务 |
| AC1 P06 | 同 UID 分属两个来源，同一实例绑定两个规则/输出 | 身份分别隔离；逻辑键包含 event、original_key、rule、output；不跨来源或输出去重 |
| AC2 P07 | 最大提前量 7 天、扩展周期 5 分钟 | 默认 30 天窗口通过；窗口短于两者之和拒绝，恰等于通过；提前量为零和越界输入分别断言 |
| AC2 P08 | 固定 now，事件位于窗口左端、右端前 1ms、右端 | 明确半开窗口；左端/右端前可规划，右端留待下轮；窗口延展不重复，窗口外事件不提前创建 |
| AC2 P09 | now 为 `2026-11-02T00:00Z`，事件在 `2026-12-01T23:59Z`，提前 7 天 | 窗口内的远期事件仍产生 `2026-11-24T23:59Z` 任务；不因错误缩短展开范围漏提醒 |
| AC2 P10 | 实例数/候选步数上限 N，输入分别需要 N 与 N+1；另测多事件累计上限 | 边界成功，超限明确失败且没有部分规划；重复/排除 RDATE 仍计入现有候选预算 |
| AC2 P11 | 假单调时钟在预算前/恰到预算/超预算，或日期运算溢出 | 建议 `elapsed >= budget` 拒绝；类型化失败、无 panic，水位及任务不部分提交 |
| AC2 P12 | 同一 floating/全天事件改解释时区，或 tzdata 版本变化后重新规划 | UTC 时间及水位版本更新，原始身份不变，旧任务作废；accepted 逻辑提醒不补发 |
| AC2 P13 | 固定时钟推进 5 分钟后重跑；另模拟 UTC 回拨/前跳而单调时钟继续前进 | 窗口可确定地延展；UTC 跳变不改变耗时预算判定，也不复制已有逻辑提醒；过期追赶由 #24 验证 |
| AC3 P14 | 一次源修订修改多个事件 | 实例、逻辑提醒、任务与水位作为同一事务可见；水位记录真实 source/rule 修订、planned_until、tzdata 版本 |
| AC3 P15 | 重复实例改期，随后修改规则或输出 generation | `original_key`/逻辑身份不变，due_at 更新；新任务 generation 单调增加，旧 pending/retry_wait/leased 作废，不能作为当前任务返回 |
| AC3 P16 | 已规划事件随后取消、删除或来源/规则/输出禁用 | 不再返回有效待发任务；相关旧任务作废；其他来源与规则不受影响；不删除 accepted 去重历史 |
| AC3 P17 | 读取修订后暂停规划，再提交较新源修订或修改规则/输出配置，然后恢复旧规划 | 拒绝过期结果，不能把旧任务标成最新；重试后水位与新输入一致；用 barrier 驱动交错，不靠 sleep |
| AC3 P18 | 分别在实例写入后、任务写入后、水位写入时注入 SQL 错误 | 每次回滚后的业务数据等于事务前快照；原事件/checkpoint 不变；允许独立记录脱敏错误，但不标记成功 |
| AC4 P19 | 同一修订/窗口重放、源无内容变化的新修订、重复延展 | 精确逻辑键集合及有效任务数不变；不只检查总数，以免错误身份替换掩盖重复 |
| AC4 P20 | 源已提交但尚未规划即关闭重开；或规划已提交但调用者未收到确认再重开重放 | 首次补齐任务；后者不复制已提交任务；完整字段与水位一致 |
| AC4 P21 | 子进程在规划事务写入后、commit 前被终止；另在 commit 后确认前终止 | 父进程重开真实 DB；前者无部分写入且可重试，后者重放幂等；用进程管道/检查点同步终止位置 |
| AC4 P22 | 预置 accepted 逻辑提醒，再改期/改规则/改输出配置并重启重放 | 保留 accepted_at 和逻辑身份，不生成该身份的新可发送任务；另一个未 accepted 实例正常重规划 |
| AC3/4 P23 | 两个规划调用基于同一修订同时提交 | 最终只有一套有效逻辑提醒/任务；另一调用幂等返回或明确冲突，不能留下混合水位与任务 |

## 事务与迁移验证

使用临时文件数据库及 `Storage::open`，通过真实 `commit_sync` 建立事件与修订；
仅为隔离 #24 而直接预置 accepted/leased 等投递状态。成功用例断言完整业务行、唯一键、
外键及修订，不以“函数返回 Ok”代替结果检查。每个用例独立数据库并关闭连接池。

新增规划表时添加迁移，不修改 `0001`。用当前 `0001` 与合成事件/日志建立旧库，
经真实入口升级；验证原数据、迁移版本、规划表约束、索引、外键、重入及重开。
失败迁移不得损坏原事件和历史。现有 foundation 测试的“只有一次迁移”断言需随真实版本更新，
不能删除重入/历史损坏验证。空库初始化不算旧版升级证据。

## 验证层与执行顺序

1. 应用单元/契约：P01–P13 的固定输入、错误及预算，拟放入
   `crates/calendar-application/tests/planning_contract.rs`；时区与重复使用小型黄金矩阵。
2. 存储集成：P14–P20、P22–P23，拟放入
   `crates/calendar-storage/tests/planning.rs`；P21 使用独立测试子进程。
   迁移升级场景加入现有 foundation 套件。
3. Red：先确定可编译的应用/仓储接口，再以最小未实现行为执行测试并保存断言失败；
   缺少接口导致编译失败、数据库夹具错误均不记为需求 Red。随后实现、Green、Refactor。
4. 检查关键断言：受控地去掉 accepted 去重、改变 original_key、提前推进水位，
   分别要求 P22、P15、P18 出现对应行为失败；恢复后重跑。构建失败不算检出。

首轮选用固定单元测试、真实 SQLite 集成、故障注入、进程恢复和迁移升级测试。
不新增统一 CI 门禁。若实现引入无法用这些测试覆盖的跨阶段协议，再单独设计有界状态模型，
不把本设计称为形式证明。

拟定本地预算：应用目标套件 10 秒，存储（含进程恢复）60 秒；超时先诊断，不算需求失败。
实施后运行 fmt、Clippy、workspace tests、Python 契约/仓库检查及 `git diff --check`，
记录实际版本、命令、用例数及 Red/Green 证据。

## 实际 TDD 记录

- Red：定义 `calendar-application` 的 `Clock`、`PlanningRequest`、`PlanningLimits`、`PlanningError`、`ReminderPlanner` 与 `0002_reminder_planning.sql` 迁移，仓储 `reconcile` 先返回最小未实现桩。运行 `cargo test -p calendar-storage --test planning`，19 项验收测试全部因行为断言失败（`0 passed; 19 failed`）。
- Green：在 `calendar-application::plan_events` 实现纯窗口、单调时钟耗时预算、跨事件累计实例数/候选步数上限及全天/相对偏移触发计算；在 `calendar-storage` 以 `BEGIN IMMEDIATE` 事务校核 source/rule/output 版本、持久化 `occurrences`/`logical_deliveries`/`delivery_jobs`/`planning_watermarks`，保留 `original_key` 与 `accepted_at` 去重并作废旧 generation。19 项存储规划测试、3 项应用契约测试及 `0001 -> 0002` 历史升级测试全部通过。
- 关键断言检出验证：受控移除 `accepted_at` 去重、将 `original_key` 替换为改期后时间、将水位更新移出实例/任务事务，分别触发 `ac4_accepted_identity_is_not_reissued_after_rescheduling_and_reopen`、`ac3_rescheduling_supersedes_old_generation_and_preserves_logical_identity`、`ac3_write_failures_roll_back_occurrences_jobs_and_watermark_together` 的预期断言失败；恢复后全量回归通过。

## Review 回归

Review 回归：`ac4_terminal_jobs_are_not_reissued_without_a_planning_change` 验证
`failed_permanent`/`expired` 在重开、重复规划及无内容变化同步后不重建任务，实际事件变化仍可生成新 generation。
`ac3_scope_components_with_colons_keep_separate_legacy_watermarks` 验证含冒号的不同来源/规则组合不共享水位，
并保留旧格式 scope_id。两项在修复前均出现行为断言失败；修复后通过。
水位 upsert 使用 `(source_id, rule_id, output_id)` 唯一键，新 scope_id 使用不透明随机 ID，不修改历史迁移。

PR #52 第一轮远端 review 的两项 P2 均先复现，再修复：

- `ac2_timezone_changes_cancel_jobs_that_leave_either_window_edge` 覆盖时区变更移出窗口左右端；
  每个任务记录解释时区与 tzdata 版本，避免另一输出更新共享实例后掩盖旧任务。
  `0003` 增加可空 `planning_context`；旧任务在重新核验后补齐，无上下文且不在当前结果中的活动任务保守取消。
  `0002 -> 0003` 真实升级/重开保留任务、accepted 历史及旧迁移 checksum。
- `ac2_budget_expiring_during_sql_writes_rolls_back_even_the_watermark` 通过 SQLite trigger + 自定义 collation
  在实例、逻辑提醒、任务及水位写入时推进假单调时钟；修复前出现 `Ok(())`，修复后返回超时且所有规划写入回滚。
  预算从仓储 reconcile 入口开始，在读取、循环、写入及 commit 前检查；SQL 单次调用和 commit 本身不被抢占，
  因此这是协作式截止检查，不宣称严格实时超时。候选展开原有数量/步数限制保持不变。

## 待确认的设计取值

- 采用既有建议：未来 30 天、每 5 分钟延展、最大提前量 7 天、全天本地 09:00；作为可配置默认值。
- 时间预算到达上限即失败；任何批次规划失败均不发布部分任务、不推进成功水位。
- 规划完成前源/规则/输出变化时拒绝旧结果并重新规划；首次版本不启用变更通知补发。

## 参考

通过 Context7 检索 SQLx 官方文档，使用其事务提交/回滚及连接配置语义设计故障验证；
实现时仍须核对仓库锁定的 SQLx 0.8.6。

- [SQLx 0.8.6 Transaction](https://docs.rs/sqlx/0.8.6/sqlx/struct.Transaction.html)：未提交事务的 drop 回滚；不能替代进程终止恢复测试。
- [SQLx 0.8.6 SQLite options](https://docs.rs/sqlx/0.8.6/sqlx/sqlite/struct.SqliteConnectOptions.html)：复用仓库 WAL、外键及 busy timeout 配置。
- [SQLx 0.8.6 LockedSqliteHandle](https://docs.rs/sqlx/0.8.6/sqlx/sqlite/struct.LockedSqliteHandle.html)：通过 Context7 查阅 `lock_handle` / `create_collation`，并核对本地固定版本源码，用于确定性注入数据库阶段的时钟变化。
- [验证工具选择](verification-toolkit.md)、[V-Model / TDD](development-method.md)：按风险选择测试并保留真实证据。
