# 运行、安全与恢复设计

## 1. Cloudflare 与管理页面

管理端采用 Cloudflare Access 认证身份与管理员允许名单，并通过 Turnstile 完成人机校验。人机验证通过不能授予管理员身份。

建议网络边界：

```text
admin.example.com -> Access 身份验证 -> Tunnel -> 管理页面 / 管理 API
feed.example.com  -> HTTPS + token   -> Tunnel -> 只读 ICS 路由
ingest.example.com-> HTTPS + API token->Tunnel -> 标准事件提交路由
```

按 Host 和路由双重限制入口；feed/ingest host 不得访问管理路由。Tunnel 容器只连接专用内部网络，源站不暴露公网端口。部署检查必须包含“无法绕过 Access 访问管理面”。若使用直接反向代理，必须额外限制源站可达性与可信代理，不能信任客户端自带 CF 请求头。

应用层验证 `Cf-Access-Jwt-Assertion` 的签名、issuer、audience、exp/nbf 和管理员身份；JWKS 受控缓存与轮换，未知 key 刷新失败则拒绝。不可只判断某个邮件头或 JWT 头存在。[Access 官方文档](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/application-token/)

流程：

1. 浏览器先完成 Access 登录，获取管理静态页面。
2. Turnstile widget 生成 token，提交到管理会话端点。
3. Rust 后端调用 Siteverify，检查 success、允许的 hostname 和预期 action；token 五分钟有效且只能使用一次。校验失败或服务不可达时不创建新会话。[Turnstile 官方文档](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/)
4. 创建绑定 Access sub 的随机会话，设置 Secure、HttpOnly、SameSite Cookie，初设最长 8 小时；每次请求同时验证 Access JWT 与会话绑定，写请求验证 CSRF 和 Origin。
5. 删除来源、轮换凭据等操作写审计记录；退出或管理员移除立即吊销本地会话。已有会话无需每请求重新做人机验证；Access 身份必须持续有效。

ICS 客户端与机器接入 API 使用各自 token，不要求交互登录或 Turnstile。管理会话、订阅 token、API token 均不复用。

## 2. 密钥与输入边界

- token 使用密码学安全随机值（至少 256 bit），展示一次，库内只保存摘要；可撤销、限作用域、可设到期时间。
- 远端鉴权凭据和 Bark device key 需要可逆使用，使用标准 AEAD 加密保存；每次加密独立 nonce，并将凭据身份与版本作为关联数据。主密钥从只读 secret 文件挂载，支持 key version 轮换。
- 备份与主密钥分开保存；主密钥遗失会导致相关凭据无法恢复。日志不输出 token、Cookie、Authorization、完整订阅 URL 或原始日历正文。
- 来源 URL、OAuth token endpoint、Bark endpoint 同样执行 SSRF 防护：默认 HTTPS、拒绝 loopback/私网/link-local/元数据地址，校验 IPv4/IPv6、DNS 解析结果及每次重定向，避免 DNS rebinding。自托管 Bark 私网访问仅允许管理员显式配置的固定主机/网段，禁止任意私网访问。
- 跨主机重定向不转发鉴权头；限制重定向次数、连接/请求时间、响应与解压后大小、页面数、事件数和重复展开预算。
- UI 对事件标题/描述做安全渲染；ICS 导出使用合规 escaping/折行，防止换行注入；模板不允许执行代码。
- API 限流、请求体限额、上传限额及登录会话端点限流。管理允许精确同源请求，不使用任意来源 CORS。
- CSP 显式允许实际使用的 Turnstile 官方资源域；订阅响应与所有代理层关闭带密钥 URL 的完整访问日志，禁用第三方分析和外链资源，设置 Referrer-Policy: no-referrer。

## 3. Docker 镜像与部署

本阶段不提供不能运行的 Dockerfile/Compose。实现时按以下契约交付可验证镜像：

| 配置 | 目标 |
| --- | --- |
| 多阶段构建 | Rust 可执行文件与 web 静态资源，不携带编译工具链 |
| 固定依赖 | Cargo.lock、前端 lockfile、基础镜像 digest；按版本发布 |
| 最小运行权限 | 非 root UID、read_only、cap_drop ALL、no-new-privileges |
| 持久化 | 整个 `/data` 目录本地持久卷，包含 SQLite DB/WAL/SHM |
| 临时目录 | `/tmp` 独立 tmpfs，限定大小 |
| 敏感配置 | `/run/secrets/*` 只读挂载，不写入镜像或前端 bundle |
| 重启 | `restart: unless-stopped`，宿主机 Docker 随系统启动 |
| 健康检查 | 镜像内提供 `calrelay healthcheck` 子命令，不依赖镜像恰好存在 curl |
| 资源 | CPU、内存、PID、连接池、队列批次和网络超时限制 |
| 停止 | SIGTERM，建议 30 秒宽限；停止领任务，结束短事务，在途结果尽量落盘 |
| 发布 | amd64/arm64 构建、镜像扫描、SBOM、迁移版本标签与升级说明 |

容器需携带 CA 根证书与可更新的时区数据；时间同步由宿主机负责。服务不挂载 Docker socket，不拥有宿主机管理权限。

SQLite WAL 需要同主机本地文件系统，不能把多节点容器共享的 NFS 当作数据库高可用方案。[SQLite WAL](https://sqlite.org/wal.html)

## 4. 健康检查与重启语义

| 检查 | 判断 | 后果 |
| --- | --- | --- |
| `/health/live` | HTTP 事件循环、规划/调度核心心跳是否前进 | 本地检查失败，需恢复或退出 |
| `/health/ready` | 迁移完成、数据库可读写、配置加载完成、核心工作器已启动 | 不接收新管理写入与 ingest |
| 功能状态 | 各来源同步时间、Bark 错误、队列延迟、磁盘容量 | 降级告警，不直接判定进程死亡 |

数据库写探测需限频、短超时，不在每次检查写大量审计。迁移期间 ready=false；live 给明确启动宽限。外部 ICS、Bark 或 Cloudflare 短时不可达不触发整个进程重启。

**Docker healthcheck 将容器标记为 unhealthy，不会单独使普通 Compose 容器重启；restart 策略作用于进程退出。** 因此采用两层机制：核心任务监督器发现不可恢复故障时安全退出非零；宿主机独立监督（如 systemd timer）在持续 live 失败后执行受控重启。后者覆盖进程完全卡死的情况，必须按实际部署系统提供配置与演练。[Docker 重启策略](https://docs.docker.com/engine/containers/start-containers-automatically/)、[Compose healthcheck](https://docs.docker.com/reference/compose-file/services/#healthcheck)

建议初始阈值：10 秒检查一次、启动宽限 60 秒、连续 6 次 live 失败触发外部监督恢复，10 分钟最多 3 次恢复，超限告警并保留现场。外部监督器运行于宿主机，由其管理 Docker；应用自身不获得该权限。

磁盘满、SQLite 损坏、无效主密钥不能靠无限重启修复：停止新写入/投递，报告故障并保留数据；不得自动删除数据库或重建空库伪装恢复成功。

## 5. 备份、升级与恢复

- WAL + synchronous=FULL、外键开启、busy timeout 初设 5 秒，小连接池；源网络获取和 Bark 推送均不占用数据库写事务。
- 按日执行 SQLite Online Backup API 或经验证的 `VACUUM INTO` 一致性备份；不能在运行期间只复制 `.db` 文件忽略 WAL。[SQLite 备份](https://sqlite.org/backup.html)
- 建议保留 7 日备份与 4 周备份，异机加密存储；备份包含配置、迁移版本、事件、任务与审计，不包含主密钥本体。实际 RPO 由成功备份间隔决定，初始目标不超过 24 小时。
- 升级前一致性备份，启动时由唯一实例执行显式迁移；迁移失败不进入 ready。恢复旧镜像不等于数据库降级，需要兼容迁移或还原匹配的备份。
- 每次恢复先在隔离环境执行完整性/外键检查、验证解密和事件数量。恢复默认关闭推送，核对时间窗口与投递历史后再启用。
- 从旧备份恢复会丢失备份后的成功投递记录，可能造成重复发送；默认禁止发送恢复启用时间之前应触发的任务，除非管理员明确选择追赶。
- 启动扫描未完成同步、规划水位和过期租约，先恢复一致性再发送任务；未完成同步的暂存记录不得充当完整快照。

## 6. 可观测性与故障演练

结构化日志携带 request_id、source_id、event_id、job_id，不包含正文或密钥。指标记录同步延迟、规划积压、到期投递延迟、投递尝试/接受/失败/未知、租约回收、SQLite busy、磁盘与备份年龄。

告警不只依赖 Bark，保留宿主机或独立渠道；内部指标端点不可公开暴露来源详情。

上线前至少演练：

1. 提交事件后、规划前强制终止，启动后仍生成正确任务。
2. Bark 接收后响应超时，保留未知结果并展示可能重复。
3. 事件改期/取消与在途发送竞争，不复活旧任务。
4. 断网、401/OAuth 失效、429、磁盘满、DB busy、损坏备份。
5. 核心工作器崩溃与整个进程卡死，分别验证内部退出与宿主机监督。
6. 源站绕过、伪造 Access 头、Turnstile 重放、订阅 token 撤销、私网 SSRF。
