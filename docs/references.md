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
| [Bark 推送文档](https://github.com/Finb/Bark/blob/master/docs/tutorial.md) | JSON POST、设备 key 与输出配置的依据 |
| [bark-server API V2](https://github.com/Finb/bark-server/blob/master/docs/API_V2.md) | 对接主动推送，应用响应需检查；不从其文档推导端到端去重或设备回执保证 |

## 安全与运行文档

| 来源 | 本项目采用方式 |
| --- | --- |
| [Turnstile 服务端校验](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/) | Context7 `/websites/developers_cloudflare_turnstile`：后端 Siteverify，检查 hostname/action；token 5 分钟、单次使用 |
| [Access 应用 JWT](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/application-token/) | 验证管理身份与源站边界；验证签名和声明，不只检查请求头 |
| [Docker restart 策略](https://docs.docker.com/engine/containers/start-containers-automatically/) | Context7 `/docker/docs`：进程退出后的恢复；启动条件和手动停止语义依文档处理 |
| [Compose healthcheck](https://docs.docker.com/reference/compose-file/services/#healthcheck) | 声明探测；不把 unhealthy 状态当作普通 Compose 自动重启机制 |
| [SQLite WAL](https://sqlite.org/wal.html) | 同主机持久卷、短事务、checkpoint；不用于多节点共享网络文件系统 |
| [SQLite Online Backup API](https://sqlite.org/backup.html) | 一致性备份；运行中不只复制主数据库文件 |

## 选型前需要补齐的验证

- 通过 Context7 或 Exa 检索候选 Rust ICS/重复/时区库的官方文档和公共测试代码，逐项核对能力；当前不指定未经验证的库。
- Turnstile 站点、Access 管理员策略、域名、Tunnel 与宿主机监督按实际部署环境补齐。
- Bark 版本差异、响应字段、限流、自托管端点及客户端通知权限需要实机联调。
- 每次重大依赖升级核对迁移、时区与日历语义变化；本文件保留稳定官方链接以便持续复查。

## GitHub 开发流程

通过 Context7 `/github/docs` 查询评审与 ruleset 参数，并核对以下官方文档：

- [Ruleset 可用规则](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)：PR、独立审批、失效审批、签名与状态检查；私有仓库可用性受套餐限制。
- [Rulesets REST API](https://docs.github.com/en/rest/repos/rules)：保护配置格式与检查来源绑定。
- [批准 PR](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/reviewing-changes-in-pull-requests/approving-a-pull-request-with-required-reviews)：作者不能批准自己的 PR；独立 reviewer 需要实际权限。
- [actions/checkout](https://github.com/actions/checkout)、[actions/setup-python](https://github.com/actions/setup-python)：核对官方发布并解析固定 commit SHA，避免可变 Action 标签。
