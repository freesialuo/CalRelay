# 项目参考

核对日期：2026-10-21。使用 Context7 查询 Cloudflare Turnstile、Docker、SQLx 文档，并直接核对官方规范、SQLite 文档与 Bark 公共代码文档。以下将规范事实与本项目决策区分；实现时继续核对依赖版本与兼容性。

## 日历规范

| 来源 | 用途与约束 |
| --- | --- |
| [RFC 5545：iCalendar](https://datatracker.ietf.org/doc/html/rfc5545) | 时间、VEVENT、重复与例外、VTIMEZONE、VALARM、ICS 序列化的主要依据；模型不是完整标准兼容承诺 |
| [RFC 8984：JSCalendar](https://datatracker.ietf.org/doc/html/rfc8984) | 参考其结构化日历表达；CalRelay EventV1 是自定义协议，不冒充 JSCalendar |

## 实现文档与公共代码

| 来源 | 本项目采用方式 |
| --- | --- |
| [SQLx SQLite 连接参数](https://docs.rs/sqlx/latest/sqlx/sqlite/struct.SqliteConnectOptions.html) | Context7 `/websites/rs_sqlx`：显式配置 WAL、外键、busy_timeout 与同步级别，避免隐式依赖默认值 |
| [SQLx 源码](https://github.com/launchbadge/sqlx) | 异步数据库访问和迁移的实现参考；正式使用前锁版本 |
| [Serde 字段属性](https://serde.rs/field-attrs.html) | Context7 `/websites/serde_rs`：核对 `default` 与 `deserialize_with`，覆盖补丁保留缺省继承与显式 null 清除的区别（#13、#14） |
| [chrono-tz 0.10.4 `GapInfo` 与 `IANA_TZDB_VERSION`](https://docs.rs/chrono-tz/0.10.4/chrono_tz/struct.GapInfo.html) | Context7 `/chronotope/chrono-tz`：核对跳变间隙起始偏移提取与内嵌 IANA 版本常量（`2025b`），用于 #19 黄金样例与水位版本固定 |
| [chrono 0.4.45 `LocalResult`](https://docs.rs/chrono/0.4.45/chrono/offset/enum.LocalResult.html) | Context7 `/websites/rs_chrono_chrono`：核对 `from_local_datetime` 在 `Single`、`Ambiguous(earliest, latest)` 与 `None` 下的语义（#19） |
| [rrule 0.14.0 文档与源码](https://docs.rs/rrule/0.14.0/rrule/)、[fmeringdal/rust-rrule](https://github.com/fmeringdal/rust-rrule) | Exa 核对 `RRuleSet`、`counter_date.rs` 与 `utils.rs`：确认其在午夜 DST 跳变、闰日递增、`RDATE PERIOD` 与 `RECURRENCE-ID` 例外上的限制，#20 据此采用领域内受限日历坐标展开 |
| [Bark 推送文档](https://github.com/Finb/Bark/blob/master/docs/tutorial.md) | JSON POST、设备 key 与输出配置的依据 |
| [bark-server API V2](https://github.com/Finb/bark-server/blob/master/docs/API_V2.md) | 对接主动推送，应用响应需检查；不从其文档推导端到端去重或设备回执保证 |

## 重复展开 review 修复（PR #42）

2026-10-22 通过 Context7 `/websites/rs_chrono_chrono` 核对 Chrono checked 运算文档，并检查本地已锁定 0.4.45 公共源码；未复制第三方实现。

| 来源 | 采用方式 |
| --- | --- |
| [RFC 5545 §3.3.10](https://www.rfc-editor.org/rfc/rfc5545.html#section-3.3.10)、[§3.8.5.3](https://www.rfc-editor.org/rfc/rfc5545.html#section-3.8.5.3) | 核对 `YEARLY` 下 `BYMONTHDAY` 的 Expand 语义，以及重复实例继承结束时间时的跨度保持与例外独立跨度 |
| [Chrono 0.4.45 TimeDelta](https://docs.rs/chrono/0.4.45/chrono/struct.TimeDelta.html#method.try_seconds)、[DateTime](https://docs.rs/chrono/0.4.45/chrono/struct.DateTime.html#method.checked_add_signed)、[NaiveDate](https://docs.rs/chrono/0.4.45/chrono/struct.NaiveDate.html#method.checked_add_days) | `try_seconds`、`checked_add_signed`、`checked_add_days` 的越界返回值用于类型化错误；源码核对 `src/time_delta.rs` 与 `src/datetime/mod.rs` |

第二轮 review 通过同一 Context7 库核对 [NaiveDate::from_ymd_opt](https://docs.rs/chrono/0.4.45/chrono/struct.NaiveDate.html#method.from_ymd_opt) 和 [Datelike::year](https://docs.rs/chrono/0.4.45/chrono/trait.Datelike.html#tymethod.year)：Chrono 的可表示年份宽于本项目 Temporal 的四位格式，因此候选日期必须先检查项目范围再格式化与解析。

第三轮 review 通过 Context7 `/chronotope/chrono-tz` 核对 [Tz::from_str](https://docs.rs/chrono-tz/0.10.4/chrono_tz/enum.Tz.html#impl-FromStr-for-Tz)：未知 IANA 名称返回错误；本项目仅在 date/floating 需要解释日历时区时调用该校验，继续优先拒绝来源 VTIMEZONE。

## 安全与运行文档

| 来源 | 本项目采用方式 |
| --- | --- |
| [Turnstile 服务端校验](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/) | Context7 `/websites/developers_cloudflare_turnstile`：后端 Siteverify，检查 hostname/action；token 5 分钟、单次使用 |
| [Access 应用 JWT](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/application-token/) | 验证管理身份与源站边界；验证签名和声明，不只检查请求头 |
| [Docker restart 策略](https://docs.docker.com/engine/containers/start-containers-automatically/) | Context7 `/docker/docs`：进程退出后的恢复；启动条件和手动停止语义依文档处理 |
| [Compose healthcheck](https://docs.docker.com/reference/compose-file/services/#healthcheck) | 声明探测；不把 unhealthy 状态当作普通 Compose 自动重启机制 |
| [SQLite WAL](https://sqlite.org/wal.html) | 同主机持久卷、短事务、checkpoint；不用于多节点共享网络文件系统 |
| [SQLite Online Backup API](https://sqlite.org/backup.html) | 一致性备份；运行中不只复制主数据库文件 |

## SQLite 存储实现

Issue #21 使用 Context7 `/websites/rs_sqlx` 核对连接选项和迁移宏，再核对下载的 SQLx 0.8.6 公共源码中的版本/校验和检查；未复制第三方实现。

| 来源 | 采用方式 |
| --- | --- |
| [SQLx 0.8.6 SQLite 连接选项](https://docs.rs/sqlx/0.8.6/sqlx/sqlite/struct.SqliteConnectOptions.html) | 显式设置文件路径、建库、WAL、FULL、外键和 busy timeout，避免依赖默认值 |
| [SQLx 0.8.6 migrate 宏](https://docs.rs/sqlx/0.8.6/sqlx/macro.migrate.html) | 内嵌版本迁移；build script 追踪新迁移文件 |
| [SQLx 0.8.6 Migrator 公共源码](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/migrate/migrator.rs) | 核对未完成、未知版本与 checksum 不匹配的失败路径；失败时关闭新池，不重建数据库 |
| [SQLx 0.8.6 事务](https://docs.rs/sqlx/0.8.6/sqlx/struct.Transaction.html)、[SqlitePool](https://docs.rs/sqlx/0.8.6/sqlx/type.SqlitePool.html) | #22 通过 Context7 核对 begin_with、commit 与 Drop 回滚，再检查 0.8.6 公共源码；使用 BEGIN IMMEDIATE 和条件 UPDATE，以故障/并发测试验证实际原子性 |
| [SQLite STRICT 表](https://sqlite.org/stricttables.html) | 核心表使用严格类型，最低 SQLite 3.37；实际随 SQLx bundled SQLite 构建 |

## 选型前需要补齐的验证

- 通过 Context7 或 Exa 检索候选 Rust ICS/重复/时区库的官方文档和公共测试代码，逐项核对能力；当前不指定未经验证的库。
- Turnstile 站点、Access 管理员策略、域名、Tunnel 与宿主机监督按实际部署环境补齐。
- Bark 版本差异、响应字段、限流、自托管端点及客户端通知权限需要实机联调。
- 每次重大依赖升级核对迁移、时区与日历语义变化；本文件保留稳定官方链接以便持续复查。

## GitHub 开发流程

通过 Context7 `/github/docs` 查询评审与 ruleset 参数，并核对以下官方文档：

- [Ruleset 可用规则](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)：PR、独立审批、失效审批与状态检查；私有仓库可用性受套餐限制。
- [Rulesets REST API](https://docs.github.com/en/rest/repos/rules)：保护配置格式与检查来源绑定。
- [批准 PR](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/reviewing-changes-in-pull-requests/approving-a-pull-request-with-required-reviews)：作者不能批准自己的 PR；独立 reviewer 需要实际权限。
- [actions/checkout](https://github.com/actions/checkout)、[actions/setup-python](https://github.com/actions/setup-python)：核对官方发布并解析固定 commit SHA，避免可变 Action 标签。
