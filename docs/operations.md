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

#26 已实现的存储边界、密钥文件格式、轮换步骤及诊断见 [凭据安全](credential-security.md)；机器写入 HTTP 与管理鉴权模块已交付；标准 binary 的完整管理 verifier/secret provider 和生产循环装配仍由 #75 交付。

- token 使用密码学安全随机值（至少 256 bit），展示一次，库内只保存摘要；可撤销、限作用域、可设到期时间。
- 远端鉴权凭据和 Bark device key 需要可逆使用，使用标准 AEAD 加密保存；每次加密独立 nonce，并将凭据身份与版本作为关联数据。主密钥从只读 secret 文件挂载，支持 key version 轮换。
- 备份与主密钥分开保存；主密钥遗失会导致相关凭据无法恢复。日志不输出 token、Cookie、Authorization、完整订阅 URL 或原始日历正文。
- 来源 URL、OAuth token endpoint、Bark endpoint 同样执行 SSRF 防护：默认 HTTPS、拒绝 loopback/私网/link-local/元数据地址，校验 IPv4/IPv6、DNS 解析结果及每次重定向，避免 DNS rebinding。自托管 Bark 私网访问仅允许管理员显式配置的固定主机/网段，禁止任意私网访问。
- 跨主机重定向不转发鉴权头；限制重定向次数、连接/请求时间、响应与解压后大小、页面数、事件数和重复展开预算。
- UI 对事件标题/描述做安全渲染；ICS 导出使用合规 escaping/折行，防止换行注入；模板不允许执行代码。
- API 限流、请求体限额、上传限额及登录会话端点限流。管理允许精确同源请求，不使用任意来源 CORS。
- CSP 显式允许实际使用的 Turnstile 官方资源域；订阅响应与所有代理层关闭带密钥 URL 的完整访问日志，禁用第三方分析和外链资源，设置 Referrer-Policy: no-referrer。

## 3. Docker 镜像与部署

Docker 是产品交付要求；#38 可提前准备 Dockerfile，但最终验收必须等真实服务、构建 TS 前端和 #78 系统闭环，不能以示例 Compose 代替。实现时按以下契约交付可验证镜像：

| 配置 | 目标 |
| --- | --- |
| 多阶段构建 | Rust 可执行文件与 web 静态资源，不携带编译工具链 |
| 固定依赖 | Cargo.lock、前端 lockfile、基础镜像 digest；按版本发布 |
| 最小运行权限 | 非 root UID、read_only、cap_drop ALL、no-new-privileges |
| 持久化 | 整个 `/data` 目录本地持久卷，包含 SQLite DB/WAL/SHM |
| 临时目录 | `/tmp` 独立 tmpfs，限定大小 |
| 敏感配置 | `/run/secrets/*` 只读挂载，不写入镜像或前端 bundle |
| 外部恢复 | 目标 restart: unless-stopped；HEALTHCHECK 持续失败后由外部执行有界终止，机制由 #38 确定，手动停止保持停止 |
| 健康检查 | 镜像内提供 `calrelay healthcheck` 子命令，不依赖镜像恰好存在 curl |
| 资源 | CPU、内存、PID、连接池、队列批次和网络超时限制 |
| 停止 | SIGTERM，建议 30 秒宽限；停止领任务，结束短事务，在途结果尽量落盘 |
| 发布 | amd64/arm64 构建、镜像扫描、SBOM、迁移版本标签与升级说明 |

容器需携带 CA 根证书与可更新的时区数据；时间同步由宿主机负责。服务不挂载 Docker socket，不拥有宿主机管理权限。

SQLite WAL 需要同主机本地文件系统，不能把多节点容器共享的 NFS 当作数据库高可用方案。[SQLite WAL](https://sqlite.org/wal.html)

## 4. 健康检查与重启语义

| 检查 | 判断 | 后果 |
| --- | --- | --- |
| `/health/live` | 真实核心 HTTP 与必需工作器进度，不以假心跳代替 | 暴露真实不健康及诊断，恢复由外部部署机制决定 |
| `/health/ready` | 迁移完成、数据库可读写、配置加载完成、核心工作器已启动 | 不接收新管理写入与 ingest |
| 功能状态 | 各来源同步时间、Bark 错误、队列延迟、磁盘容量 | 降级告警，不直接判定进程死亡 |

探针复用同一核心 runtime，不创建第二套服务/调度循环，不写敏感数据；真实 HTTP/DB/必需工作器进度不得用独立 ticker 假冒。HEALTHCHECK CLI 尽量只读；DB 读写就绪性可读取同一 runtime 的有界背景探测/真实写入进度，必要写探测限频、短超时、非敏感且不在每次 CLI 检查产生写入。迁移期间 ready=false；live 给明确启动宽限，心跳/宽限/探测截止使用单调时钟。外部 ICS、Bark 或 Cloudflare 短时不可达不触发整个进程重启；部分接入器按设计间歇上线，应按能力展示来源状态，不能成为全局不健康理由。

**Docker healthcheck 将容器标记为 unhealthy，不会单独使普通 Compose 容器重启；restart 策略主要响应进程退出。** 核心只负责暴露真实健康/组件诊断，本次不新增主动退出、自重启或恢复控制策略。已有正常关闭/实际崩溃的退出行为保留；持续不健康或完全卡死后的恢复由系统边界外的部署机制负责，具体控制器、阈值、限次与告警选型待 #38 确定。核心不调用 Docker API、不持有 Docker socket 或宿主机权限。[Docker 重启策略](https://docs.docker.com/engine/containers/start-containers-automatically/)、[Compose healthcheck](https://docs.docker.com/reference/compose-file/services/#healthcheck)

磁盘满、SQLite 损坏、无效主密钥不能靠无限重启修复：停止新写入/投递，报告故障并保留数据；不得自动删除数据库或重建空库伪装恢复成功。

## 5. 备份、升级与恢复

- WAL + synchronous=FULL、外键开启、busy timeout 初设 5 秒，小连接池；源网络获取和 Bark 推送均不占用数据库写事务。
- 按日执行 SQLite Online Backup API 或经验证的 `VACUUM INTO` 一致性备份；不能在运行期间只复制 `.db` 文件忽略 WAL。[SQLite 备份](https://sqlite.org/backup.html)
- 建议保留 7 日备份与 4 周备份，异机加密存储；备份包含配置、迁移版本、事件、任务与审计，不包含主密钥本体。实际 RPO 由成功备份间隔决定，初始目标不超过 24 小时。
- 升级前一致性备份，启动时由唯一实例执行显式迁移；迁移失败不进入 ready。恢复旧镜像不等于数据库降级，需要兼容迁移或还原匹配的备份。
- 每次恢复先在隔离环境执行完整性/外键检查、验证解密和事件数量。恢复默认关闭推送，核对时间窗口与投递历史后再启用。
- 从旧备份恢复会丢失备份后的成功投递记录，可能造成重复发送；默认禁止发送恢复启用时间之前应触发的任务，除非管理员明确选择追赶。
- 启动扫描未完成同步、规划水位和过期租约，先恢复一致性再发送任务；未完成同步的暂存记录不得充当完整快照。

实际本地维护入口、隔离恢复步骤和故障验证见 [一致性备份与隔离恢复](backup-recovery.md)。

## 6. 可观测性与故障演练

结构化日志携带 request_id、source_id、event_id、job_id，不包含正文或密钥。指标记录同步延迟、规划积压、到期投递延迟、投递尝试/接受/失败/未知、租约回收、SQLite busy、磁盘与备份年龄。

告警不只依赖 Bark，保留宿主机或独立渠道；内部指标端点不可公开暴露来源详情。

上线前至少演练：

1. 提交事件后、规划前强制终止，启动后仍生成正确任务。
2. Bark 接收后响应超时，保留未知结果并展示可能重复。
3. 事件改期/取消与在途发送竞争，不复活旧任务。
4. 断网、401/OAuth 失效、429、磁盘满、DB busy、损坏备份。
5. 核心 HTTP/DB/工作器失败与停滞，验证真实健康暴露；进程实际崩溃/完全卡死的外部恢复由部署层按选定机制演练，不要求核心新增主动退出策略。
6. 源站绕过、伪造 Access 头、Turnstile 重放、订阅 token 撤销、私网 SSRF。

## 7. 配置权威与迁移（目标）

本表区分 main 已有配置和 #73 待冻结目标，不是一份随意添加的通用 env 清单。env 不能覆盖 UI 保存的业务设置；不明项保持待确定，冻结时列出每项名字/类型/默认与拒绝行为，不能隐含扩张白名单。配置文件不含真实密钥。

| 配置项 | 唯一权威与入口 | 持久性与生效 | 敏感性与迁移状态 |
| --- | --- | --- | --- |
| 监听地址 | 部署层显式配置文件，禁止 UI/env | 部署文件持久，变更重启 | 非密钥但不可回显不可信配置；main 的 CALRELAY_BIND 待 #73 迁移/移除，旧项明确拒绝并给升级诊断；不预选固定地址/端口或另设监听 env |
| CALRELAY_DATABASE | 当前部署 env，仅选择本地持久 DB 路径 | 部署配置持久；启动读取，变更需停机迁移/重启 | 路径不作业务配置；目标是否继续保留 env 由 #73 冻结，不能 UI 切库 |
| CALRELAY_MAX_REQUEST_BYTES | 当前部署 env；目标仍部署资源上限，不是 UI 业务值 | 当前启动读取，修改重启 | 非密钥；允许范围与默认以 config.rs 为准，最终白名单待 #73 确认 |
| CALRELAY_MAX_RESPONSE_BYTES | 当前部署 env；部署响应预算 | 同上 | 非密钥；不能因 UI 修改增大部署安全上限，最终 env 保留待冻结 |
| CALRELAY_REQUEST_TIMEOUT_MS | 当前部署 env；部署请求总时限 | 同上 | 非密钥；区别来源业务刷新周期，最终 env 保留待冻结 |
| CALRELAY_INGEST_RETENTION_MS | 当前部署 env；机器幂等记录保留预算 | 当前启动读取；重启不抹已保存记录 | 非密钥；缩短对重放/容量影响须明示，最终入口/迁移待 #73 冻结 |
| CALRELAY_INGEST_MAX_RECORDS | 当前部署 env；机器幂等容量预算 | 当前启动读取，修改重启 | 非密钥；不能充当来源数量业务开关，最终 env 保留待冻结 |
| 来源/adapter 能力、周期、URL、日历选择、规则、输出模板、订阅策略 | UI→管理 API→SQLite 是唯一业务权威，不接受启动 env 覆盖 | DB 持久；成功事务后以配置 revision 生效，旧 fetch/plan/send/cache fence 失效，无需重启 | URL/配置可能含敏感内容，凭据须独立引用；可逆凭据不放 config_json，删除保留审计/accepted 历史 |
| 远端凭据、Bark key、producer/feed token | 凭据提供器与受保护管理流程；token 只展示一次，DB 保存摘要或 AEAD 密文 | DB 持久；撤销/轮换即时影响后续验证，不能从幂等/缓存旁路 | 敏感；禁止 env 正文/URL/日志/bundle；挂载主密钥不在 DB/备份中 |
| 主密钥文件与版本选择 | 部署只读 secret 文件是密钥权威；provider 装配由 #75 | 文件部署持久；当前文件格式见 credential-security；热轮换/重启策略由 #73/#75 冻结 | 高敏感；具体配置入口名字待确定，不杜撰 env |
| Access issuer/audience/管理员允许名单、Turnstile hostname/action/sitekey/secret | 管理安全 bootstrap 的部署信任配置；管理 UI 不自行扩大授权 | 部署配置持久，当前仅显式 with_admin 装配；变更生效与会话撤销规则待 #73/#75 冻结 | sitekey 可公开；secret 独立只读文件，其余身份配置也不入公共导出；不改真实账号或安全设置 |
| worker 心跳阈值/启动宽限/探测预算 | #36 与 #75 的监督部署契约，唯一入口待冻结 | 是否动态或重启待冻结，不与业务周期混用 | 非密钥；不得以配置替代真实进度检测；保留 #71 已有成果 |

#73 先发布配置迁移/兼容与失效契约，再由后续实现移除 runtime 的监听 env；本 PR 不改现有启动行为。恢复 pause/cutoff 是 DB 持久安全状态，普通启动 env、UI 配置保存、探针或监督不能悄悄复位，显式恢复授权流程独立验证。

## 8. Docker 恢复语义与验收

目标部署组合：Dockerfile HEALTHCHECK 调用镜像自带 CLI 的 live 检查；启动宽限后连续核心失败达到阈值，外部恢复执行者先 SIGTERM、等待有限宽限，必要时 SIGKILL；`restart: unless-stopped` 保持非手动停止实例的重启能力。HEALTHCHECK 不负责 kill，核心不主动操纵 Docker。恢复资格依据持续核心 liveness 故障与真实组件证据；readiness=false 单独不足以触发终止，恢复暂停、迁移/配置未就绪及来源 freshness 均不得被当成 kill 理由。外部执行者必须识别启动中/暂停/维护状态并遵守手动停止 fence，避免恢复暂停造成重启循环。具体外部机制/权限方案尚未选型，不默认引入 autoheal sidecar 或给主服务 socket。见[部署 UML](product-design.md#uml-部署恢复时序目标外部执行者待选型)。

| 核对项 | 事实、推断与实现边界 |
| --- | --- |
| HEALTHCHECK | interval/timeout/start-period/retries 控制探测和连续失败状态，不自行重启；探针只报告真实健康，不能把普通来源离线计为核心失败。[Dockerfile 官方说明](https://docs.docker.com/reference/dockerfile/#healthcheck) |
| unless-stopped | 手动停止保持停止，含 daemon 重启后的该状态；进程退出的重启与健康状态本身是不同机制。[Docker restart policy](https://docs.docker.com/engine/containers/start-containers-automatically/) |
| docker stop | 默认先 SIGTERM，再按停止宽限升级 SIGKILL；是手动停止操作，不能当作自动恢复触发器。[stop 官方说明](https://docs.docker.com/reference/cli/docker/container/stop/) |
| docker kill API | 发送所选信号不等于无副作用的 OS kill：核对的 Moby killWithSignal 会设置 HasBeenManuallyStopped；restartmanager 对 unless-stopped 检查该标记，不能承诺 API kill 后自动重启。[kill 说明](https://docs.docker.com/reference/cli/docker/container/kill/)、[固定源码 kill.go](https://github.com/moby/moby/blob/b711d21792db6db488de976eb3c4a653c92c3bc3/daemon/kill.go#L89)、[restartmanager](https://github.com/moby/moby/blob/b711d21792db6db488de976eb3c4a653c92c3bc3/daemon/internal/restartmanager/restartmanager.go#L78) |
| 直接进程信号 | 不经 Docker kill API 的进程终止与上述手动标记不同；可触发非手动退出重启是基于源码的推断，受实际 Engine/进程身份与启动条件约束。外部路径须防 PID 复用、旧容器 generation、手动 stop 竞争；选型后实际验收，不在本文提供可直接执行的 kill 命令 |

核对日期 2026-10-25，Moby 快照 `b711d21792db6db488de976eb3c4a653c92c3bc3`；这不是目标运行环境版本承诺。本轮未连接 Docker daemon，也未运行容器/信号实验。#38 必须固定测试 Engine/Compose/镜像版本，并证明：

- deliberate manual stop 及 daemon 重启后仍停止，外部执行者不会 start/revive，含 stop 与恢复并发竞争。
- 启动宽限与瞬时失败不终止；连续核心失效才恢复，来源/Bark 离线不触发全局恢复。
- 真实进程崩溃与完全卡死分别覆盖，SIGTERM 宽限、必要 SIGKILL、冷却/限次/告警和失败现场可审计。
- 对比 stop、kill API 与所选进程信号路径的实际 restart-policy 行为，不把信号名称相同当成语义相同。
- 重启后配置、事件修订、水位、租约回收与 accepted 去重保留；restore pause/cutoff 不自动解除，不补发已禁止的历史任务。
