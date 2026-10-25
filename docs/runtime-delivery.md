# 有界 due 投递工作器（#75 第一批）

`calendar-server::delivery_worker` 将已有 `deliver_one` 组合为真实队列循环。它需要显式提供输出、Clock、worker 身份和预算；输出 port 必须按 envelope.output_id 分发所有已配置输出，领取不是按输出过滤的，不能为每个 BarkOutput 各启一个全局领取器。标准 binary 的安全分发与正式注册由[生产运行时](production-runtime.md) 负责。不新增业务配置、迁移、mock 生产来源或默认免鉴权。

`DeliveryWorkerLimits::new` 限制每轮 1–256 个操作、1ms–120s 单调准入预算、1ms–60s 扫描间隔。每次操作单独经过 `Services::run_task`，复用 claim/prepare/finish 的持久租约、generation/revision/freshness fence、accepted 去重、取消竞争审计与重启恢复。网络仍在事务外；该模块不绕过 Bark 的 HTTPS/SSRF/credential 边界。

持久恢复状态读取后再次检查单调期限，避免查询耗时越过预算后仍开始领取。轮次到达上限或单调预算后停止新准入，已开始的操作继续执行到 finish；UTC 跳变不改变轮次耗时预算。每轮之后有可被停止通知打断的等待，包含满轮次，避免持续忙转。存储错误返回调用方；外部渠道结果继续沿用持久重试，不能仅因输出离线就将核心判为故障。

`DeliveryProgress::{Processed, Empty, RecoveryPaused}` 只在真实操作完成后调用观察者，不在等待时打 heartbeat。Processed 只表示处理了领取结果，不表示 Bark 接受或设备展示。只读 `Storage::delivery_paused` 将恢复暂停与数据库故障分开；最终租约/cutoff guard 仍由 SQLite 强制，worker 从不写 pause/cutoff。恢复策略仍只允许隔离/停服运维操作更改。

正常停止通知禁止后续准入，允许已开始的有界操作保存结果；外层 supervisor 负责有限 drain。该工作器本身不持有整条循环的 task permit，不控制进程退出或 Docker。强制取消时可能留下已有发送意图/租约，其恢复与 unknown 审计仍由下次 claim 处理，不声称外部 exactly-once。

## 验证与依赖

集成测试使用真实 SQLite 与合成配置，仅测试中提供受控输出/本地模拟 HTTP。覆盖有界轮次、单调预算、停止排空、空查询、存储失败、取消后 accepted 审计、重开去重、真实 backup/restore 的 pause/cutoff。Bark 实际适配器仍使用已有本地 TLS 模拟响应矩阵测试。有效 Red：未检查轮次操作上限时实际处理 3 项，断言预期 1 项失败；恢复暂停最初误走 claim 导致 Database，增加只读状态判别后通过。没有生产网络或设备发送。

此批已正常组合 main 上的 #71 监督、#96 admin bootstrap、#97 规则/输出配置查询、#98 来源扫描及 #101 汇总订阅；不重复这些模块的 DTO、迁移或 API。监督集成测试直接注册真实 due-loop：空 SQLite 查询报告 Completed；真实 backup/restore 后报告 RecoveryPaused、保持 pause/cutoff 且不领取；卡住的测试输出触发有限 drain，重开 SQLite 后发送意图/租约仍未完成，没有伪造 accepted。缺失其他生产工作器时 ready 仍为 false，健康诊断不关闭业务准入。

标准 binary 已通过[生产运行时](production-runtime.md) 组合实际配置查询、窗口/水位与 Bark 安全 provider；本模块不拥有配置或 supervisor。#90 只验收投递基础，#75/#74 的完整产品与部署 AC 仍按各自要求验收。

参考：[Tokio watch Receiver](https://docs.rs/tokio/1.54.1/tokio/sync/watch/struct.Receiver.html) 的订阅/changed 语义与取消安全；[Tokio sleep](https://docs.rs/tokio/1.54.1/tokio/time/fn.sleep.html) 的异步等待与取消语义。实现使用仓库固定 Tokio 1.54.1。
