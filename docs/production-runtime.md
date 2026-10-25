# 首版生产运行时（#107，部分推进 #75）

标准 binary 在监听前装配实际 source / planner / delivery 三工作器。部署 JSON 必须同时包含已验证的 `admin` 与 `credentials`；缺失任一项会在创建数据库/监听前失败。坏 secret/key 沿用已有安全 bootstrap 的有界文件读取与脱敏错误。没有默认 key、mock 配置或免鉴权开关。

按 [admin bootstrap](admin-auth.md) 与 [Bark 凭据初始化](credential-bootstrap.md) 准备部署文件、只读 secret 挂载和显式本地 provision-bark 命令。网页只引用 CLI 返回的 credential_id，配置来源、规则、输出及订阅均复用已有管理 API 和共享 DTO。启动仍为：

```sh
CALRELAY_DATABASE=/data/calrelay.sqlite3 calendar-server --deployment-config /run/calrelay/deployment.json
```

`healthcheck live/ready --deployment-config FILE` 只读探测，不打开主密钥或数据库；`credential ...` 本地 CLI 不监听，不加载 Turnstile secret。库的 `DeploymentConfig::initialize` 保留显式嵌入用途；生产入口 `production_runtime::initialize` 执行完整安全要求。HTTP/业务准入与健康 readiness 仍独立，外部来源/Bark 离线不会停止 HTTP 或主动退出、重启。

## 实际工作与预算

- 来源：复用能力感知 SourceScanner、受限 HttpFetcher、ICS 解析与 synchronize。每轮至多一页100来源、8次操作，最多追踪16384个来源；轮次准入2秒，轮后等待1秒，单次 HTTP 上限20秒，失败退避1–300秒。manual-static/push-only/退休/禁用来源不轮询，旧配置在网络前/提交时有既有 revision fence。
- 规划：通过 ReminderConfigurationRepository 分页发现当前 source/rule/output scope，每页至多100项，每轮至多8个 reconcile，保留未处理页。30日滚动窗口、5分钟延展阈值、最大提前7日；每次展开最多10000 occurrence、100000 iteration、1秒预算。来源/规则/输出版本及实际 TZDB_VERSION 与水位不符时重新规划。已有持久 planned_until 在重启后直接复用；UTC 前跳到阈值或后跳越过当前 horizon 时重新规划。只读 planning_required 是调度提示，最终检查与 jobs/watermark 原子提交仍由既有 reconcile 完成。
- 投递：单一全局 due 循环，每轮最多8项、2秒新准入预算；恢复状态查询后再次检查期限。每次 envelope 通过 output_for_delivery 查询当前 generation 后创建既有 BarkOutput，不缓存配置/secret。输出自己的 request_timeout_ms 是额外上限，整个 deliver_one 仍受30秒全局 deadline约束，租约60秒。结果先持久化，再报告真实进度；SQLite Busy/StaleLease 退到下一轮，其他核心失败交监督诊断。

这些是首版资源预算，没有新增业务配置 schema 或监听环境变量。单调时钟控制准入和等待，UTC 只用于事件/租约/规划语义。等待不产生 heartbeat；空发现、空领取、完成工作、恢复暂停与外部结果均来自真实操作。empty configuration 表示已装配的空队列，不表示已有可投递的业务配置。

监督 stall 预算120秒，覆盖单次来源/投递请求及存储收尾；SIGTERM 沿用10秒 graceful drain 及同长度强制取消清理窗口。轮次预算仅限制新准入，不提前丢弃在途结果。超时取消仍可能留下发送意图/租约，重启领取沿用 unknown/accepted 去重规则；不保证外部 exactly-once 或设备展示。

三个工作器在实际恢复暂停时均不开始业务轮次，只报告 RecoveryPaused；不修改 pause/cutoff，不自动 resume。数据库最终 lease/cutoff trigger 仍为投递权威。恢复策略只由隔离/停服运维流程更改。

## 验证与剩余边界

测试使用临时 SQLite、合成部署 secret/key、安全 provider、loopback HTTP/模拟输出，覆盖三工作器真实 empty ready、规划水位/延展/版本、分页待处理预算、prepare 后输出变更失效、真实 backup/restore pause、SIGTERM 与重启。网络组件组合测试使用仅测试的 loopback transport，生产 HTTPS/SSRF 与实际 Bark TLS/响应矩阵继续由 adapters 现有测试验证；未给生产适配器添加私网豁免。

仅启用 production-web（未启用 test-harness）的最终标准 binary 另经真实子进程测试：显式凭据 CLI 创建加密 handle；按现有配置 API 持久化来源/规则/输出并提交事件；实际 source 刷新与 Bark 目标的私网拒绝不关闭 HTTP，planner 生成水位/任务，投递 attempt 完整入库；同进程 /admin/、受保护 API、live/ready 均按契约响应，SIGTERM 后 SQLite 重开保持结果与零 accepted。成功 ICS/304/503 及 Bark 重试/accepted/重开去重在 loopback 模拟 transport 组合测试中覆盖，没有访问生产。

同镜像 UI 静态托管/Docker 打包复用已合并 #103。本批不修改 assets server、Docker 恢复或 supervisor 内部，不关闭 #75/#74/#36 的其他 AC。此文档必须与正式基线 CI 和依赖合并状态一起验收，隔离分支不能视为已发布闭环。

## #78 隔离系统验收入口

显式构建 `test-harness` feature 后，`Server::with_test_runtime_ports<R: SourceRunner + Send + 'static, O: PushOutput + Send + 'static>(runner, output)` 复用生产的同一 source/planner/delivery 循环、真实时钟、预算及停止协议。它要求已装配 admin 与实际 KeyRing provider；调用者可先使用 `DeploymentConfig::initialize_with_supervision(production_runtime::supervision_budgets())`，按独立 harness 的既有方式装配本地 verifier，再调用该方法。Bark handle 由正式本地 CLI 创建，`server.services.credential_provider()` 返回可克隆的真实解密 provider。

来源可传入 `IcsSourceRunner<仅测试的 RemoteFetcher>`；输出传入仅测试的 PushOutput，并按 envelope.job_id/output_id 通过已有 output_for_delivery 查询当前配置/credential handle，再访问 loopback 模拟 Bark。mock 只替代外部 transport，不复制 planner/领取循环或构造生产 fake 数据。现有生产适配器的 TLS/SSRF 验证保留独立矩阵；该 harness 证据不冒充标准 image 的外部连接验收。

测试 `harness_ports_run_the_actual_three_loops_with_mock_http_and_secure_provider` 证明三循环实际持续运行：loopback ICS 自动同步、配置驱动规划、Bark503后自动重试/accepted，以及正常停止。标准入口始终调用 `with_production_runtime`，不接受生产 mock 字段、环境变量或测试路由；仅 `production-web` 构建不包含该注入入口。
