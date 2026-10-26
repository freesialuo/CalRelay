# 实施路线与架构决策

## 首个 alpha 对账（2026-10-26）

本节覆盖下文早期规划中的实现状态，核对基线为 main `55bfd041`（已合并 #117/#118）；原 Issue AC 不删减，开放不等于没有交付。当前只建议 #2 在维护者确认后关闭，其余保留剩余验收。

已发布的 `ghcr.io/freesialuo/calrelay:v0.1.0-alpha.1` 固定源提交 `acdc38e286eb141eab017174840c7c993899cd56`，digest `sha256:478d9ef2d2f87993e8babeee286fa0a8ef0abf681f6ee5bb1a8a884a52d902be`；[只读诊断](https://github.com/freesialuo/CalRelay/actions/runs/40244469650)证实包为 private、关联本仓库，config 与原测试镜像匹配。main 新增的 #117/#118 验收代码不在该旧镜像内。本轮不重发、覆盖、更新 latest 或部署服务器。

| Issue / 范围 | 已交付证据（均为已合并 PR） | 剩余原 AC / 结案状态 |
| --- | --- | --- |
| [#2 领域与时间语义](https://github.com/freesialuo/CalRelay/issues/2) | AC1–5：#17/#41/#42/#51；四类时间、语义拒绝、黄金样例与 schema 契约 | 建议父任务确认后关闭；本轮尚未关闭。高级未支持语义仍显式拒绝，不等于全部 RFC 能力。 |
| [#3 标准源与接入](https://github.com/freesialuo/CalRelay/issues/3) | ICS 适配/安全获取/原子协调 #54/#56/#57；机器事件 API #68；周期扫描 #98 | 保留开放：标准源 envelope 的完整执行入口、更多协议/分页增量组合及外部 producer 系统负例，见 #73/#74/#78。 |
| [#5 ICS / Bark 输出](https://github.com/freesialuo/CalRelay/issues/5) | renderer #64、Bark #66、单源 feed #70、多源 feed #101；auth-before-cache、mock 响应与 round-trip 测试 | 保留开放：AC1 的时间/例外能力须按 renderer 明示支持矩阵核对；真实客户端互操作和系统取消竞争未由成功链证明，不宣称设备已显示。 |
| [#6 管理与安全](https://github.com/freesialuo/CalRelay/issues/6) | 鉴权 #69、安全 bootstrap #96、凭据 CLI #104、配置 UI #94/#109、编译静态资产 #103 | AC1 的诊断/审计管理闭环仍待验收；诊断 UI/API 由并行 owner 推进，当前不算完成；完整绕过/失败系统矩阵仍见 #78。 |
| [#7 Docker / 运行恢复](https://github.com/freesialuo/CalRelay/issues/7) | 监督 #71、备份恢复 #67、真实工作器 #112、amd64 镜像 #110、CD #111 | AC3 外部监督崩溃/卡死与手动停止演练、AC4 升级/恢复、AC5 磁盘满/损坏故障仍需完整证据；部分适配器离线不导致核心重启。 |
| [#9 分支归档流程](https://github.com/freesialuo/CalRelay/issues/9) | 独立流程议题；本轮未操作分支、归档、自动删除或仓库设置 | AC1–4 尚未完整核对；不作为首个 alpha 业务完成证明，不以文档清理改变既有设置。 |
| [#35 管理全旅程](https://github.com/freesialuo/CalRelay/issues/35) | 来源、规则、输出、汇总订阅的真实 API/UI 与类型/构建 #94/#109 | AC1 诊断/审计、AC2 的全部能力/未知结果呈现和变更竞争系统验收仍待完成；保留原 AC 与子项，不将父项缩为空壳。 |
| [#36 真实核心监督](https://github.com/freesialuo/CalRelay/issues/36) | typed supervisor/HTTP/DB probe/CLI #71；三真实 worker 注册与进度 #112；测试生命周期 #117 | AC1–3 基础已装配，不再以缺 runtime 阻塞；仍需按原 AC 对启动宽限、真实 worker 停滞/退出及完整 CLI/降级故障证据逐项验收，外部恢复由 #38/#78 跟踪。 |
| [#38 最终容器交付](https://github.com/freesialuo/CalRelay/issues/38) | #110 非 root、多阶段、内嵌网页、健康 CLI、Compose 和 amd64 smoke；#111/CD 首个私有 alpha 已存在 | AC1 arm64 未验收；AC2 升级恢复、AC3 外部监督/有界恢复/手动停止与告警、AC4 扫描/SBOM/迁移演练未完整满足。HEALTHCHECK 不自动重启。 |
| [#73 持久配置 / 标准源契约](https://github.com/freesialuo/CalRelay/issues/73) | 来源 #82、规则/输出 #97、汇总配置 #101、bootstrap #96、UI #109；监听 env 已移除 | AC2/5 的完整 envelope HTTP snapshot/delta/基线/重放协议和 browser producer 系统负例未完成；机器单事件 PUT/DELETE 不能冒充新 envelope 执行入口。 |
| [#74 能力感知周期同步](https://github.com/freesialuo/CalRelay/issues/74) | #98 scanner 与 #112 生产注册；pull 刷新、隔离退避、CAS fence，static/push-only 不假轮询 | AC1 手动刷新及更完整标准源 envelope 执行、AC5 上传替换入口/更多接入器与完整竞争验收仍缺；当前 ICS pull/snapshot 仅单日历。 |
| [#75 中心核心运行时](https://github.com/freesialuo/CalRelay/issues/75) | #88 due 投递、#96 安全 bootstrap、#104 CLI、#112 三循环、#117 受控运行时 ports | AC2–4/6–7 的完整 DST/UTC 跳变、在途取消审计可见性、重启/恢复/停滞系统矩阵尚未全部验收；已交付真实运行时，不再写 admin=None 或假 worker。 |
| [#76 多来源汇总](https://github.com/freesialuo/CalRelay/issues/76) | #101 配置与 feed、#109 UI、#118 双来源相同外部 UID 独立导出、200/304 | AC1–3 的改期/取消、选择收窄/旧缓存竞争与完整陈旧/失败组合仍需按原 AC 汇总验收；稳定身份与 auth-before-cache 基础已实现。 |
| [#77 实际 TS 用户旅程](https://github.com/freesialuo/CalRelay/issues/77) | #94/#109 CRUD、正式 API/nonce、共享类型；#118 首次登录至两源/规则/输出/订阅链路 | AC1 上传/其他可执行 adapter 与诊断、AC2 inflight/unknown 审计呈现、AC4 producer 权限负例仍不完整；未来能力不显示为已可用入口。 |
| [#78 系统验收](https://github.com/freesialuo/CalRelay/issues/78) | #114 夹具、#117 typed lifecycle/clock、#118 浏览器→HTTP→SQLite→真实三循环→模拟 Bark→聚合成功链 | AC1 异构能力/push/重启，AC2 变更竞争，AC3 完整 DST/强杀/恢复，AC4 缓存竞争，AC5 producer 负例，AC7 核心故障矩阵仍未完整；AC6 已有可复现命令与边界。 |

证据入口：[领域能力](domain-capabilities.md)、[源配置](source-configuration.md)、[周期扫描](runtime-source-scanning.md)、[生产运行时](production-runtime.md)、[核心监督](core-supervision.md)、[汇总订阅](aggregate-feed.md)、[系统验收与未验证矩阵](system-acceptance.md)。系统成功链使用合成身份和受控网络，不证明真实 Cloudflare 账号、Bark 设备展示、所有日历客户端行为或宿主机恢复策略。

首个 alpha 限制：单实例 SQLite；ICS pull/snapshot 单日历，static/push-only 不轮询；完整通用 envelope 执行/上传/更多 adapter、诊断审计 UI、arm64 与完整故障矩阵继续跟踪。双向远端写回、多实例、任意进程内插件和跨源智能合并属于后续能力，不偷偷加入首版。#4/#15 已关闭成果保持，不重开；本轮不变更原生 parent/dependency 或归档分支。

## GitHub 工作项

| Issue | 交付范围 |
| --- | --- |
| [#1](https://github.com/freesialuo/CalRelay/issues/1) | 架构基线、开发流程与 CI |
| [#2](https://github.com/freesialuo/CalRelay/issues/2) | Rust workspace、事件领域与时间语义 |
| [#3](https://github.com/freesialuo/CalRelay/issues/3) | 标准日历源协调与 ICS/HTTP 等协议适配器 |
| [#4](https://github.com/freesialuo/CalRelay/issues/4) | SQLite 修订日志与可恢复调度 |
| [#5](https://github.com/freesialuo/CalRelay/issues/5) | ICS 订阅与 Bark 输出器 |
| [#6](https://github.com/freesialuo/CalRelay/issues/6) | TypeScript 管理面与 Cloudflare 安全 |
| [#7](https://github.com/freesialuo/CalRelay/issues/7) | Docker、健康监督与备份恢复 |

## 可独立验收的后续工作

父项保留整体目标，子项通过 GitHub 原生层级关联，具体依赖及验收标准以各 Issue 为准。#11–#14、#16 已由 #17 合并；#15 兼容性评估与 #4 仓储/调度基础已验收关闭，保持成果不重开。#2 父项仍 open，不在本设计未经整体验收关闭。

| 父项 | 子项与交付结果 |
| --- | --- |
| #15 库评估 | [#18 ICS](https://github.com/freesialuo/CalRelay/issues/18)、[#19 时区](https://github.com/freesialuo/CalRelay/issues/19)、[#20 重复展开](https://github.com/freesialuo/CalRelay/issues/20) |
| #4 存储与调度 | [#21 连接/迁移/约束](https://github.com/freesialuo/CalRelay/issues/21)、[#22 来源原子提交](https://github.com/freesialuo/CalRelay/issues/22)、[#23 规划水位](https://github.com/freesialuo/CalRelay/issues/23)、[#24 租约与恢复](https://github.com/freesialuo/CalRelay/issues/24) |
| #3 接入 | [#25 ICS 归一化](https://github.com/freesialuo/CalRelay/issues/25)、[#28 安全 HTTP](https://github.com/freesialuo/CalRelay/issues/28)、[#29 同步协调](https://github.com/freesialuo/CalRelay/issues/29)、[#30 标准接入 API](https://github.com/freesialuo/CalRelay/issues/30) |
| #5 输出 | [#31 ICS 渲染](https://github.com/freesialuo/CalRelay/issues/31)、[#32 订阅权限与缓存](https://github.com/freesialuo/CalRelay/issues/32)、[#33 Bark 适配器](https://github.com/freesialuo/CalRelay/issues/33) |
| #6 服务、安全与管理 | [#26 凭据/token](https://github.com/freesialuo/CalRelay/issues/26)、[#27 服务生命周期](https://github.com/freesialuo/CalRelay/issues/27)、[#34 管理鉴权](https://github.com/freesialuo/CalRelay/issues/34)、[#35 管理闭环](https://github.com/freesialuo/CalRelay/issues/35) |
| #7 运行与恢复 | [#36 健康监督](https://github.com/freesialuo/CalRelay/issues/36)、[#37 备份恢复](https://github.com/freesialuo/CalRelay/issues/37)、[#38 容器部署](https://github.com/freesialuo/CalRelay/issues/38) |

#21 已由 #39 合并，#22 已由 #40 合并（见 [来源同步](source-sync.md)），#19 已由 #41 合并。#20 已由 #42 合并，交付重复展开黄金样例评估与受限展开边界（见 [领域能力](domain-capabilities.md)），#23 在此基础上实现持久化实例、逻辑提醒去重与规划水位事务（见 [提醒规划](reminder-planning-test-design.md)）；#24 实现任务租约、重试及进程终止后恢复（见 [投递恢复](delivery-recovery.md)）；#26 实现凭据 AEAD、密钥版本轮换与独立机器 token（见 [凭据安全](credential-security.md)）；后续 #25/#28/#29/#30/#31/#33/#34/#27/#37 也已合并，模块交付不等于生产闭环。用户故事、开发准入和验证约定见 [开发方法](development-method.md)。#9 的分支归档流程保持独立，不与业务实现混合。

[#45](https://github.com/freesialuo/CalRelay/issues/45) 为已合并 #42 引入局部 Kani 证明与故障注入检查，范围和执行见 [形式验证](formal-verification.md)。

#18 的 ICS 候选评估见 [解析库评估](ics-parser-evaluation.md)，选择 `ical 0.11.0` 作为低层解析候选；#25 在 `calendar-adapters` 中实现受限 ICS 文件归一化与快照批次构造（见 [ICS 归一化](ics-normalization.md)）。

#28/#56 已合并受限 HTTPS 获取（见 [远程获取](remote-fetch.md)）；#29/#57 已合并分页协调和原子提交（见 [来源协调](source-coordinator.md)）。#30/#68 机器事件 API、#34/#69 管理鉴权、#32/#70 单源 feed 与 #36/#71 监督均已合并；#96/#112 已完成标准 binary 安全装配和真实三工作器。当前剩余范围以本页首个 alpha 对账为准。

## 决策基线

| 编号 | 决策 | 原因与代价 |
| --- | --- | --- |
| ADR-001 | 模块化单体，Rust + TypeScript | 事务与部署简单，模块边界仍需严格维护 |
| ADR-002 | 自定义 EventV1，参考 iCalendar/JSCalendar | 保留领域语义，需维护规范映射与能力清单 |
| ADR-003 | SQLite 单节点持久化 | 自托管成本低，接受单写者和单节点限制 |
| ADR-004 | 持久化修订日志 + 数据库任务队列 | 重启可恢复，需规划水位、租约与幂等约束 |
| ADR-005 | ICS 拉取与主动推送分离 | 不把客户端刷新当作可靠定时投递 |
| ADR-006 | Access 身份 + Turnstile + 应用会话 | 人机验证和管理员授权各有独立职责 |
| ADR-007 | 可重试投递，不承诺端到端 exactly-once | Bark 无已核验的幂等/终端回执保障 |
| ADR-008 | 标准日历源契约统一内置协议 adapter 与外部 producer | 语言无关通过鉴权协议接入，不允许任意进程内代码执行；#30 是现有基础 |

上述是既定选型与本轮补充目标；具体能力以已合并实现及验收为准，未实现目标不标完成。

## 阶段 0：模型与兼容性验证

- 项目名已确定为 CalRelay，remote 为 `https://github.com/freesialuo/CalRelay.git`；许可证由项目所有者选择。
- 建立 Rust workspace、前端、锁文件和 CI；实现 EventV1 结构及语义验证器。
- 使用本仓库 schema 和 example 建立跨语言契约；定义错误码及能力清单。
- 对候选 ICS/时区/重复库做黄金样例验证，锁定版本；若无法满足时间语义，先收窄能力并明确拒绝。

验收：时间类型不丢失，单次/全天/夏令时/重复例外可解释；不支持的输入有明确诊断。

## 阶段 1：可靠最小闭环

- ICS 单次事件 + 鉴权 HTTP 获取；标准事件 upsert/delete。
- SQLite 迁移、源快照事务、修订日志、规划水位、任务租约。
- 固定提前量 Bark、稳定 UID 的只读 ICS 订阅、发送记录。
- 事件改期/取消、重启恢复、过期追赶规则、凭据和订阅 token 轮换。

验收：重复同步不重复造任务；取消作废未发任务；崩溃后可恢复；Bark 不确定结果不误报设备已送达。

## 阶段 2：完整管理与部署

- TypeScript 管理面：来源、日历、规则、输出、订阅、诊断与审计。
- Access + Turnstile + 会话、SSRF 防护、API token 作用域和请求限额。
- 可运行 Dockerfile/Compose、持久卷、健康检查、外部监督样例、备份恢复流程。

验收：容器升级数据保留；healthcheck 与重启分别演练；外部渠道失败不引起重启风暴；管理身份和订阅权限互不混用。

## 阶段 3：日历与来源扩展

- RRULE、EXDATE、RDATE/PERIOD、单次例外、THISANDFUTURE、自定义时区逐项启用。
- OAuth2、分页/增量源、声明式 JSON 映射、多日历过滤。
- 导入警报执行策略、每日摘要、免打扰、变更通知及其他输出器。

验收：每项能力有真实协议样例与故障样例；导出后再次导入关键语义一致；同一输入在不同宿主机时区得到相同规划结果。

## 质量与暂缓项

允许按 Issue 风险与 TDD 需要引入的测试、证明与质量能力见 [工具选择矩阵](verification-toolkit.md)；能力目录不等于当前全部已接入，不默认扩大强制 CI。

核心时间逻辑做性质测试与黄金样例；数据库并发约束和崩溃恢复做集成测试；ICS 做语义 round-trip；网络输出使用模拟服务验证超时/限流/响应丢失。接入与输出真实凭据只用于明确授权的人工联调。

暂缓：CalDAV 双向同步、多租户、多实例调度、任意进程内插件、跨源智能合并。需要时另立 ADR，不提前增加消息中间件。

## #72 设计时的产品集成顺序（历史规划）

以下保留当时的依赖意图，不表示这些 PR 当前仍未合并；当前实现与剩余 AC 以本页首个 alpha 对账为准。

[DFD/UML、标准日历源和 AC 映射](product-design.md)明确已实现基础/待集成目标；[配置权威表](operations.md#7-配置权威与迁移目标)记录监听 env 的迁移冲突。早期阶段列表保留目标，并非当前完成状态。

1. #73 冻结来源/能力/envelope 与全部最小持久配置、管理 API、配置权威及迁移，作为单一 owner；#35 原 AC 全保留。
2. 按冻结契约推进 #74 能力感知同步、#75 中心核心服务 composition root、真实循环与工作器注册、#76 多来源汇总（另等 #32）、#77 实际 TS 管理旅程。可以在明确接口边界并行，不修改别人未合并 PR。
3. #36 保留 #71 已有 CLI/typed supervisor/probe 成果，真实工作器注册前不结案；#78 进行真实浏览器/HTTP/SQLite/周期与模拟 Bark 的完整系统验收。
4. #38 可早准备镜像构建，但最终镜像/升级/宿主监督验收等 #78/#36/#37/#35，镜像必须含真实服务与构建前端；部署采用 HEALTHCHECK、持续核心失效后的外部有界终止与 unless-stopped，手动停止保持停止；stop/kill API 与直接进程信号的语义须按指定 Engine 实验验收，具体外部执行者待选型。

设计轮次曾通过通用 GET 核实原生关系：当时 47 个非 PR Issue 中的既有 35 条依赖边及合入本轮规划后的图均无环；#35 parent=#6、#36/#38 parent=#7，新 #72–#78 暂无原生关系。当前可用关系写入接口不足，新增原生关系尚未创建，不能以正文替代。保留全部既有 parent，不提前关闭未验收事项，不合并 PR。
