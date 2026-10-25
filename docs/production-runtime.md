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

生产同镜像 UI 静态托管/Docker 打包由 #103 单独负责。本批不修改 assets server、Docker 恢复或 supervisor 内部，不关闭 #75/#74/#36 的其他 AC。此文档必须与正式基线 CI 和依赖合并状态一起验收，隔离分支不能视为已发布闭环。
