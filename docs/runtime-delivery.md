# 有界 due 投递工作器（#75 第一批）

`calendar-server::delivery_worker` 将已有 `deliver_one` 组合为真实队列循环。它需要显式提供输出、Clock、worker 身份和预算；输出 port 必须按 envelope.output_id 分发所有已配置输出，领取不是按输出过滤的，不能为每个 BarkOutput 各启一个全局领取器。标准 binary 尚未注册，不能据此声称生产闭环已完成。不新增业务配置、迁移、mock 生产来源或默认免鉴权。

`DeliveryWorkerLimits::new` 限制每轮 1–256 个操作、1ms–120s 单调准入预算、1ms–60s 扫描间隔。每次操作单独经过 `Services::run_task`，复用 claim/prepare/finish 的持久租约、generation/revision/freshness fence、accepted 去重、取消竞争审计与重启恢复。网络仍在事务外；该模块不绕过 Bark 的 HTTPS/SSRF/credential 边界。

轮次到达上限或单调预算后停止新准入，已开始的操作继续执行到 finish；UTC 跳变不改变轮次耗时预算。每轮之后有可被停止通知打断的等待，包含满轮次，避免持续忙转。存储错误返回调用方；外部渠道结果继续沿用持久重试，不能仅因输出离线就将核心判为故障。

`DeliveryProgress::{Processed, Empty, RecoveryPaused}` 只在真实操作完成后调用观察者，不在等待时打 heartbeat。Processed 只表示处理了领取结果，不表示 Bark 接受或设备展示。只读 `Storage::delivery_paused` 将恢复暂停与数据库故障分开；最终租约/cutoff guard 仍由 SQLite 强制，worker 从不写 pause/cutoff。恢复策略仍只允许隔离/停服运维操作更改。

正常停止通知禁止后续准入，允许已开始的有界操作保存结果；外层 supervisor 负责有限 drain。该工作器本身不持有整条循环的 task permit，不控制进程退出或 Docker。强制取消时可能留下已有发送意图/租约，其恢复与 unknown 审计仍由下次 claim 处理，不声称外部 exactly-once。

## 验证与依赖

集成测试使用真实 SQLite 与合成配置，仅测试中提供受控输出/本地模拟 HTTP。覆盖有界轮次、单调预算、停止排空、空查询、存储失败、取消后 accepted 审计、重开去重、真实 backup/restore 的 pause/cutoff。Bark 实际适配器仍使用已有本地 TLS 模拟响应矩阵测试。有效 Red：未检查轮次操作上限时实际处理 3 项，断言预期 1 项失败；恢复暂停最初误走 claim 导致 Database，增加只读状态判别后通过。没有生产网络或设备发送。

此批基于 main，可独立集成。生产装配仍有以下真实依赖，不关闭 #75 或 #74：

- #82 的共享来源配置 DTO/持久化及 `--deployment-config` 监听迁移（读取实际 PR，不复制其 DTO/0011/API）。
- 规则/输出绑定的有界配置发现 port，包含 source/rule/output 当前 revision/generation 与规划水位；真实 `PlanningRequest` 需由这些值构造。
- Bark 输出配置的 endpoint、credential 引用与 enabled/generation 契约；credential 加密存储不能代替明确的部署主密钥/只读 secret provider 装配。
- 管理 Access verifier、Turnstile 和管理员身份的严格部署配置；标准 binary 必须 fail closed，不能用 `admin=None` 宣称管理可用。
- #71 的 `with_worker` / `WorkerContext` 注册；将真实进度与 recovery_paused 接到监督，保持 readiness 与业务准入分开。仅在依赖实际合并后组合 main 并运行最终基线 CI。

后续叶子继续交付能力感知来源扫描、规划窗口延展与持久水位、安全 composition root；本批不提供虚假的空配置循环。

参考：[Tokio watch Receiver](https://docs.rs/tokio/1.54.1/tokio/sync/watch/struct.Receiver.html) 的订阅/changed 语义与取消安全；[Tokio sleep](https://docs.rs/tokio/1.54.1/tokio/time/fn.sleep.html) 的异步等待与取消语义。实现使用仓库固定 Tokio 1.54.1。
