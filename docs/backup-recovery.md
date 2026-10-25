# 一致性备份与隔离恢复（#37）

`calendar-storage` 的 `calrelay-maintenance` 是本地维护入口，不启动 server、不读取密钥文件、不执行网络投递。源库可以处于单节点 WAL 服务中；恢复与 resume 必须在隔离目录、没有使用该恢复库的服务或工作器时运行。不会替换正在服务的数据库，不自动清库或删除失败产物。

## 操作

```sh
cargo build -p calendar-storage --bin calrelay-maintenance --locked
# 源数据库必须存在，目标目录必须不存在，父目录须已存在。
target/debug/calrelay-maintenance backup /data/calendar.sqlite3 /backup/run-2026-10-25
# cutoff 为运维核实的恢复启用时刻 UTC Unix 毫秒；示例仅为参数格式。
target/debug/calrelay-maintenance restore /backup/run-2026-10-25 /isolated/recovery-1 1792886400000
```

备份目录包含 `snapshot.sqlite3` 与 `manifest.json`：后者包含格式版本、从快照读取的已应用迁移版本和整个文件的 SHA-256。所有已应用迁移的成功状态和 checksum 必须匹配本二进制对应版本的嵌入迁移，未知版本、失败迁移及修改 checksum 均拒绝。支持已知迁移集合的备份（独立迁移可按合并顺序补应用）；恢复先校验再在候选库执行剩余迁移。0009 独立接在 0006 后，0007/0008 留给并行工作，未添加空迁移。

备份使用独立只读连接的参数化 `VACUUM INTO`，包含 WAL 已提交事务而不包含未提交写入。成功前执行 `integrity_check` 和 `foreign_key_check`，同步文件、写入 manifest 并同步目录。失败可能保留 `.partial`；只有命令成功且最终 manifest 存在才算完成。SHA-256 用于意外损坏检测，不提供签名或抵御能同时修改备份与 manifest 的攻击者。只接受可信、静止的备份制品；不把任意攻击者提供的 SQLite 文件当成安全输入沙箱。

恢复独占创建目标目录，复制到 `database.partial` 后先核对完整文件摘要、迁移历史、完整性和外键，再升级候选库。恢复策略与任务状态同事务写入，关闭连接并同步后发布 `database.sqlite3`。现有目录（含符号链接）、现有原库永不覆盖。Unix 目录模式 0700、创建文件 0600；非 Unix 平台需运行环境保证等价 ACL。目录 fsync 的持久性依赖操作系统、文件系统和硬件，不宣称已证明掉电安全。恢复命令失败时目录内任何产物均隔离保留，不启动服务；调查空间/权限/版本，选择另一个新目录重试。

## 检查和解除暂停

恢复库的 `recovery_control.paused=1`，cutoff 持久保存。正常库 0009 的初始状态为 `paused=0, cutoff=NULL`。guard 在原有短事务中禁止恢复暂停时的 leased 更新/插入（包括 prepare 的租约延长），使部分 claim/send intent 回滚。暂停时 DeliveryRepository 可返回分类 `Database` 错误；维护期间不要启动投递扫描器紧密重试。无需改变 server 生命周期或业务路由。

cutoff 之前的 pending/retry_wait/leased 任务置 expired，之后的非终态任务变为 retry_wait，旧 owner/token/until 清除。未完成 attempt 按是否已有发送意图记 not_sent/unknown；accepted 与已完成历史不改。后续规划插入或更新的旧 due_at 任务同样被触发器置 expired，避免重新规划绕过 cutoff。等于 cutoff 的任务保留资格；二次恢复不能降低已有 cutoff。

继续前逐项核实：事件/来源数量与同步水位、已接受及未知投递历史、当前 UTC 与 cutoff、来源新鲜度、输出配置、凭据版本及独立密钥可解密。旧备份会回滚备份之后的 token 撤销和凭据轮换；需要按实际情况显式撤销/轮换并保留仍被引用的旧密钥。数据库含事件、配置、token 摘要与加密凭据，仍属于敏感备份。主密钥必须独立受控保存，备份程序没有密钥参数，也不复制相邻文件或环境变量。

```sh
# 仅在上述隔离核实后显式执行；不会移除 cutoff。
target/debug/calrelay-maintenance resume /isolated/recovery-1/database.sqlite3
```

resume 不安装恢复库、不切换生产数据库、不启动服务，也不授予历史追赶。服务切换由运维在停服并保留原库的独立流程完成。历史追赶属于额外显式决策，本入口不提供清除 cutoff 或批量复活旧任务的选项。备份后的成功投递可能缺失，不能承诺 exactly-once；cutoff 前历史通过默认抑制降低重复风险，未来任务仍遵循既有租约/去重契约。

## 验证证据与边界

| AC | 可执行证据 |
| --- | --- |
| AC1 | `tests/backup.rs`：活跃 WAL 的已提交/未提交事务、manifest、损坏摘要、未知迁移和孤儿外键拒绝；`backup_policy.rs`：v6→v9 升级、正常库不暂停 |
| AC2 | `backup_policy.rs`：跨重开暂停、cutoff 等号与未来任务、旧租约 prepare 拒绝、重新规划不能复活历史；`backup.rs`：独立随机密钥与加密凭据恢复 |
| AC3 | `backup.rs`：真实子进程 OS 文件大小限制中断备份/恢复写入，原库/备份不变且无成功发布；`backup_policy.rs`：受限 max_page_count 触发真实 SQLITE_FULL（13），事务回滚并重开验证原数据/任务/完整性 |

风险导向 TDD：初始成功桩下两个测试实际失败（缺失源未拒绝、成功无 manifest），实现后同组 Green。独立审查发现取消中在途 attempt 被误记 expired，新增回归实际失败后修复：保留 cancelled/superseded 等终态 disposition，清理所有旧租约。其余回归按边界扩充。满页演练覆盖 SQLite 事务失败，OS 写入限制覆盖维护入口中断；两者不等同于穷尽生产文件系统 ENOSPC、掉电或硬件失效。仅使用临时合成文件/事务/随机测试密钥，无真实服务器或通知。0007/0008 尚无合并实现，组合升级须在集成时验证，不能把当前 v6→v9 当作该组合证据。未运行长时 fuzz、形式证明或真实生产切换。

复现：`cargo test -p calendar-storage --test backup --test backup_policy --locked`，完整 gate 依 CONTRIBUTING.md。

参考：通过 Exa 阅读 [SQLite VACUUM INTO](https://sqlite.org/lang_vacuum.html#vacuum_with_an_into_clause)，核对一致快照、目标空文件及中断/同步语义；完整性/外键分开检查依 [SQLite PRAGMA](https://sqlite.org/pragma.html#pragma_integrity_check)。实现 API 同时核对锁定 SQLx 0.9.0 源码；没有引入新的 SQLite 或加密依赖。
