# 实施路线与架构决策

## GitHub 工作项

| Issue | 交付范围 |
| --- | --- |
| [#1](https://github.com/freesialuo/CalRelay/issues/1) | 架构基线、开发流程与 CI |
| [#2](https://github.com/freesialuo/CalRelay/issues/2) | Rust workspace、事件领域与时间语义 |
| [#3](https://github.com/freesialuo/CalRelay/issues/3) | ICS 与鉴权 HTTP 接入协调器 |
| [#4](https://github.com/freesialuo/CalRelay/issues/4) | SQLite 修订日志与可恢复调度 |
| [#5](https://github.com/freesialuo/CalRelay/issues/5) | ICS 订阅与 Bark 输出器 |
| [#6](https://github.com/freesialuo/CalRelay/issues/6) | TypeScript 管理面与 Cloudflare 安全 |
| [#7](https://github.com/freesialuo/CalRelay/issues/7) | Docker、健康监督与备份恢复 |

## 可独立验收的后续工作

父项保留整体目标，子项通过 GitHub 原生层级关联，具体依赖及验收标准以各 Issue 为准。#11–#14、#16 已由 #17 合并；#2 的兼容性评估仍未完成。

| 父项 | 子项与交付结果 |
| --- | --- |
| #15 库评估 | [#18 ICS](https://github.com/freesialuo/CalRelay/issues/18)、[#19 时区](https://github.com/freesialuo/CalRelay/issues/19)、[#20 重复展开](https://github.com/freesialuo/CalRelay/issues/20) |
| #4 存储与调度 | [#21 连接/迁移/约束](https://github.com/freesialuo/CalRelay/issues/21)、[#22 来源原子提交](https://github.com/freesialuo/CalRelay/issues/22)、[#23 规划水位](https://github.com/freesialuo/CalRelay/issues/23)、[#24 租约与恢复](https://github.com/freesialuo/CalRelay/issues/24) |
| #3 接入 | [#25 ICS 归一化](https://github.com/freesialuo/CalRelay/issues/25)、[#28 安全 HTTP](https://github.com/freesialuo/CalRelay/issues/28)、[#29 同步协调](https://github.com/freesialuo/CalRelay/issues/29)、[#30 标准接入 API](https://github.com/freesialuo/CalRelay/issues/30) |
| #5 输出 | [#31 ICS 渲染](https://github.com/freesialuo/CalRelay/issues/31)、[#32 订阅权限与缓存](https://github.com/freesialuo/CalRelay/issues/32)、[#33 Bark 适配器](https://github.com/freesialuo/CalRelay/issues/33) |
| #6 服务、安全与管理 | [#26 凭据/token](https://github.com/freesialuo/CalRelay/issues/26)、[#27 服务生命周期](https://github.com/freesialuo/CalRelay/issues/27)、[#34 管理鉴权](https://github.com/freesialuo/CalRelay/issues/34)、[#35 管理闭环](https://github.com/freesialuo/CalRelay/issues/35) |
| #7 运行与恢复 | [#36 健康监督](https://github.com/freesialuo/CalRelay/issues/36)、[#37 备份恢复](https://github.com/freesialuo/CalRelay/issues/37)、[#38 容器部署](https://github.com/freesialuo/CalRelay/issues/38) |

#21 已由 #39 合并，#22 已由 #40 合并（见 [来源同步](source-sync.md)），#19 已由 #41 合并。#20 已由 #42 合并，交付重复展开黄金样例评估与受限展开边界（见 [领域能力](domain-capabilities.md)），为 #23 的持久化实例与提醒规划水位提供领域基线；其余实现项仍按依赖逐项交付。用户故事、开发准入和验证约定见 [开发方法](development-method.md)。#9 的分支归档流程保持独立，不与业务实现混合。

[#45](https://github.com/freesialuo/CalRelay/issues/45) 为已合并 #42 引入局部 Kani 证明与故障注入检查，范围和执行见 [形式验证](formal-verification.md)。

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
| ADR-008 | 先固定内置适配器，后扩展外部接入 API | 减少任意代码执行和插件 ABI 复杂度 |

以上均为本次设计建议。项目所有者可在实现前修改；名称与 remote URL 不影响领域设计。

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

核心时间逻辑做性质测试与黄金样例；数据库并发约束和崩溃恢复做集成测试；ICS 做语义 round-trip；网络输出使用模拟服务验证超时/限流/响应丢失。接入与输出真实凭据只用于明确授权的人工联调。

暂缓：CalDAV 双向同步、多租户、多实例调度、任意进程内插件、跨源智能合并。需要时另立 ADR，不提前增加消息中间件。
