# 核心监督与健康探针（#36，装配/配置契约待集成）

本分支实现监督状态机、实际 HTTP/DB 探测、typed worker 注册与故障/关闭监督、healthcheck CLI。**生产来源扫描、规划请求构造和投递输出尚未装配，#36 仍未完整验收，不可提前关闭。** 测试中的合成 worker 不代表生产运行时。标准日历源可以经不同接入器进入系统，ICS 只是其中一种接入方式。

## 需求变更来源

原 #36 AC1/AC3 保留；原 AC2 的“核心不可恢复错误强制非零退出”已由用户澄清、修订后的 Issue #36/#75 与已合并设计 PR #79 撤回，改为真实健康暴露，恢复由外部部署层 #38 决定。本实现删除暂停前原型的自动 stop/退出策略，原型不作为本批授权依据。设计来源见 [product-design.md](product-design.md#中心核心服务与保活边界) 与 [operations.md](operations.md#4-健康检查与重启语义)。

## 实际组件与共享接口

`Server::initialize` 校验配置并通过 `Storage::open` 完成迁移后，才构造健康状态、绑定监听。迁移失败没有可用的健康端点，进程退出非零；外部监督必须为初始化阶段保留启动宽限。`serve` 监督真实 Axum HTTP future、HTTP/DB probe future，以及显式注册的实际 worker future。HTTP 心跳仅在本机请求已装配的 `/api/v1/version` 并得到 200 响应头后推进；没有为 HTTP 或 worker 独立更新心跳的 ticker。

生产装配可复用以下 API：

- `Server::initialize_with_supervision(Config, SupervisionConfig)`：传入 validated 单调时间预算；原 `initialize` 使用默认值。
- `Server::with_worker(WorkerKind, factory) -> Result<Server, RegistrationError>`：factory 收到 `WorkerContext`，返回 `Send + 'static` 的 `Future<Output=Result<(), WorkerFailure>>`；注册同一角色两次失败。factory 在受监督任务内调用，它本身 panic 也可被观察。
- 三个必需角色为 `SourceSync`（标准日历源接入协调扫描）、`Planner`（规则/输出及水位规划扫描）、`Delivery`（到期任务调度/投递）。没有可绕过这些角色的空必需集合。
- `WorkerContext::progress(Progress::{Completed, ExternalDegraded, RecoveryPaused})` 只能在真实有界操作完成、空队列/暂停检查完成，或外部失败已实际观察后调用。注册不等于启动进展；整段长时间 sleep、挂起网络、panic 或任务返回都不能通过 ticker 掩盖。
- `WorkerContext::{recovery_paused, cancelled}` 提供已观察到的暂停状态（首次探测前为 None）和幂等停止通知。生产循环必须通过既有 `Services::run_task` 准入每轮有界应用操作；持久恢复 guard 始终是最终边界，不得仅信任缓存暂停状态后绕过事务校验。
- `Services::health.snapshot()` 返回 typed 的 phase、live/ready、DB 状态、缺失/未完成初始操作的 worker、外部降级与核心故障。`subscribe()` 用于观察状态变化。HTTP 端点仅返回既有简短成功状态或统一错误 envelope，不暴露上述内部详情。

缺口：`ReminderPlanner::reconcile(PlanningRequest, Clock)` 是单次操作，尚缺来源/规则/输出枚举、完整请求构造、延展水位与公平轮询；`deliver_one` 尚缺生产 PushOutput/Bark、凭据与有界扫描装配；来源 `synchronize` 尚缺生产接入器配置枚举/周期执行装配。后续运行时 Issue 由父任务按端到端 User Story 协调，本分支不实现完整生产调度器。#69 管理认证已合并并完整保留；feed 由其独立 PR 交付，本分支不改变它们的语义。

## live / ready / 降级 / 停止

| 情况 | live | ready | 进程行为 |
| --- | --- | --- | --- |
| 已迁移，实际 HTTP 心跳在前进，但缺少必需生产 worker | 成功 | 503 | 不以缺失装配触发重启；明确 NotReady |
| 已注册，尚无初始实际进展，仍在启动宽限内 | 成功 | 503 | 容忍初始启动，不能伪装就绪 |
| HTTP 与所有必需 worker 有新鲜真实进展、DB 成功、无暂停 | 成功 | 成功 | Running |
| 已观察外部服务失败，worker 仍有真实有界进展 | 成功 | 成功 | 标记 ExternalDegraded；不退出/重启 |
| DB busy/locked、连接获取超时或本轮 DB probe 超时 | 成功（核心仍前进） | 503 | 有界/限频重试，释放写锁后恢复 |
| 恢复持久暂停，检查循环仍前进 | 成功 | 503 | RecoveryPaused；不领取/发送，不 resume |
| 已注册核心任务意外结束、返回错误、panic 或心跳停滞 | 503 | 503 | 只记录真实故障并保持 HTTP/API；不调用 stop、不退出、不重启。退出/panic 诊断保留；停滞后真实进展可恢复健康 |
| SIGTERM/Ctrl-C/并发 stop 正常关闭 | 关闭监听前保持成功 | 503 | 通知所有任务，排空已准入操作，关闭池后零退出 |
| 显式关闭后的排空预算耗尽 | 503 | 503 | 取消剩余 async future，结束已请求的关闭；这不是健康判定触发的终止 |

`Services::ready()` 保留原有业务准入含义（尚未停止），与新的健康 ready 分离。缺少 worker 或恢复暂停不会擅自改变 #30 ingest / #34 管理鉴权语义；未来管理配置/写路由的策略由对应契约对齐。健康发现不自动 stop 或改变业务准入；只有显式关闭调用 stop，禁止新的 `run_task` 操作。最终安全装配与 readiness 下的写入策略由 #73/#75 接入，本部分不重定义 #30/#34 的准入语义。健康失败继续使用全局错误边界、请求限额、协议头与 request_id；没有新增路由或重写 OpenAPI 路径集合。

启动宽限只容忍**尚无初始心跳**；已经有过进展的组件即使仍处启动宽限内，也在停滞预算耗尽时失败。所有预算使用 `tokio::time::Instant`/Duration，健康判断不读取 UTC；墙钟变化不参与重启决策。业务 due_at、恢复 cutoff 的 UTC 契约不变。部分接入器间歇上线正常；SourceSync 表示始终可调度的中心扫描器，不是要求每个外部连接一直在线。来源/Bark 离线只形成 ExternalDegraded，真实有界扫描/空队列检查仍能推进核心进度。完全卡死或阻塞运行时线程无法承诺进程自救，恢复由部署项决定；这里不部署镜像/Compose。实际 HTTP serve future 返回/异常已失去监听能力，沿用原生命周期的终止返回；正常信号和已请求关闭的超时亦沿用生命周期语义。探针、worker 故障或 ready=false 本身不进入该路径。

## DB probe 与恢复边界

单个 probe 循环限频执行，DB/HTTP 请求各有短超时。DB 使用 `BEGIN IMMEDIATE`、读取恢复策略、`UPDATE recovery_control SET paused=paused WHERE id=1`，核验一行并 rollback；测试实际主库可写性，不只验证 TEMP 内存表。没有新迁移、探针状态表、持久审计或自动修复。rollback 完成后才报告 DB 成功。busy/locked 与超时仅使 DB 不就绪；损坏、只读、磁盘/其他 SQL 错误、丢失策略行或池异常暴露为核心健康故障，仍保持 HTTP 运行，不自动终止或修复。后续真实探测成功可恢复健康。取消事务沿用锁定 SQLx 的 drop/rollback 行为。

探针不设置 paused、不清除 cutoff、不触碰任务/attempt；监督和重启没有 resume 路径。恢复库经实际监督循环及两次重开仍保持暂停、历史抑制和零新 attempt；显式离线 resume 的既有维护契约继续有效。读写探测本身也受暂停回归约束。

默认 typed 预算：启动宽限 60s、停滞 30s、探测间隔 1s、单次探测超时 1s、关闭 10s。宽限范围 (0,300s]，其余 (0,120s]；停滞须至少为 `interval + 2 * probe_timeout`，避免正常 DB/HTTP 有界轮次触发误判。这些值可通过 typed assembly API 配置；**本分支不新增生产环境变量绑定**，最终持久配置/CLI 绑定由配置契约任务决定。

## healthcheck CLI 与待迁移点

`target/debug/calendar-server healthcheck [live|ready]` 默认 live。探针不加载数据库配置、不打开/迁移存储、不启动服务或 worker。本机数值地址、无代理/重定向、请求总超时，仅 HTTP 200 响应头为成功；不读取或输出响应正文。非 200、协议错误、连接失败或超时非零退出，不依赖 curl。复用锁定 reqwest 0.12.24 与 SQLx 0.9.0，无依赖全量升级。

**待配置契约迁移：当前兼容代码仍从既有 CALRELAY_BIND 取得监听/探针目标，并将通配监听映射到相应 loopback；这与用户明确的“监听地址不属于环境变量”最终要求冲突。唯一配置 owner 在 #80 实现显式 `--deployment-config` JSON 并拒绝 CALRELAY_BIND；该 PR 尚未合入，本分支不另创配置解析或替代监听环境变量，最终 CLI 对接留待最小集成。** CLI 当前 `CALRELAY_HEALTHCHECK_TIMEOUT_MS`（默认 2000、范围 1–120000）也需随环境变量白名单/配置入口统一核对。当前保留实现供本地验证，不提供最终部署用的环境变量示例。

## 验证与剩余验收

- 有效 Red→Green：未实现 CLI 时真实子进程误启动服务并因缺少数据库配置失败；固定 ready 时新回归实际得到 200 而预期 503。实现后相同断言通过。按修订后的 AC2 新增回归又实际检出旧原型自动停止业务准入；移除健康驱动的 stop/退出后该回归 Green。
- 生产状态机直接接受测试提供的单调 Instant，确定性覆盖宽限前/精确边界、已开始进展在宽限内停滞、无装配不重启、注册不等于进展、外部降级、恢复暂停与粘滞故障。
- `tests/supervision.rs`：真实 Axum/SQLite、合成任务的结束/Err/panic/停滞、核心健康故障的 live 503/请求 ID、DB 写锁超时与恢复、并发 stop、关闭预算取消、暂停后不变 cutoff，以及仅本地模拟 HTTP 503。worker/DB 健康失败后 HTTP 与业务准入仍保持，真实进展或测试操作者解除 DB 故障后健康可恢复；没有自动修复或任务重启。
- `tests/process.rs`：真实标准 server 子进程遇到合成 SQL 故障后持续提供 live 503，不退出、不删除/修复故障对象；独立测试子进程通过公共装配 API 注入停滞工作器，观察 live 503、进程继续运行，直到实际 SIGTERM 正常退出，已提交数据和完整性保留。生产二进制没有测试故障开关。正常 SIGTERM 与数据库重开继续通过。
- `tests/recovery_lifecycle.rs`：真实备份恢复、历史任务、pause/cutoff、两次 server 启停，现已运行真实 HTTP/DB probe；显式 resume 后仅 cutoff 当时/之后任务能领取。CLI 的路径/失败状态/协议/超时使用临时 loopback peer。

仍未验收：生产接入协调/规划/投递装配下的完整 live/ready/功能降级，以及监听/配置契约迁移后的最终 CLI。本 PR 仅交付健康基础，不把合成 worker 测试或当前独立监督框架当作端到端生产提醒服务。未运行生产外部服务、真实账号、远端部署、长时 fuzz 或形式证明。

参考：通过 Exa 查阅 Tokio 官方 [Instant 单调时间](https://docs.rs/tokio/latest/tokio/time/struct.Instant.html)、[pause/advance](https://docs.rs/tokio/latest/tokio/time/fn.pause.html)、[JoinSet 完成/取消](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html)。生产单调状态机采用显式 Instant 注入测试，不依赖 SQL I/O 的暂停时钟自动推进；SQLx API 同时核对本仓库锁定源码的 begin_with/Transaction rollback。
