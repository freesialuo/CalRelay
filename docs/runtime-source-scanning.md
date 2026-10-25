# 有界来源周期扫描（#95 / #74 第一批）

`calendar-server::source_worker::{SourceScanner, IcsSourceRunner}` 消费 main 已合并的唯一 `SourceConfigurationRecord/Page`，复用 `HttpSource`、受限 ICS 解析、`synchronize` 和 SQLite 完整快照 CAS。不复制 DTO、迁移、管理 API 或 supervisor。#95 仅关闭此叶子，#74 的手动更新、多 calendar URL 映射、标准 envelope 执行和最终生产组合仍待后续验收。

## 调度与边界

每页沿用配置发现最多100摘要；每轮最多1–16页、1–64次同步、1ms–120s单调准入预算。游标保存在实例中：尝试数耗尽时只推进到已处理的来源，下轮继续，不跳过当前页剩余来源。完整遍历后从头再扫；并发配置变更不提供跨页快照，下轮收敛。缓存容量显式限制1–16384个活跃 pull 来源，超限返回类型化 Capacity，不能悄悄淘汰失败来源然后忙重试。单实例顺序执行，无无界任务派生或并发；多扫描器竞争仍由原 CAS 防止迟到提交，不声称分布式协调。

只对 enabled、非退休、pull/ICS/snapshot 配置调用同步；static、push-only、disabled 和 retired 不 fetch。单URL多 calendar 的协议语义尚未冻结，本叶子明确返回 Unsupported 并退避，不任选 calendar、不将局部结果当全源快照。selected 仍是输出选择，不作为是否保存源数据的开关。

进程内期限使用注入 Clock 的单调时间；成功后从操作完成起等待配置周期，失败独立指数退避并封顶。调度等待为1ms–60s且可被停止通知打断；退避基数至少等于扫描等待、上限最多1小时。数量/耗时预算只限制新准入，不中断已准入同步。实际网络/解析/提交继续受既有 RemoteFetchLimits、IcsLimits、SyncLimits 和 SQLite busy 预算限制；HTTP仍在事务外。根装配必须让真实单次同步与等待预算适配 supervisor stall_timeout，不能用等待 timer 伪造进度。

重启从持久 last_verified_at 重建剩余周期：过去核验时间用 max(0, interval-age)，未来UTC核验时间保守等待一个完整周期；没有可信核验则在有界轮次内首次同步。进程内 UTC 前后跳不改变期限。失败次数/退避不持久化；重启重新从可信核验基线开始，仍受分页、单次同步、顺序并发和轮次等待约束，不假称持久失败调度或 exactly-once GET。

## fence、鉴权与进度

配置 record 的 source_revision 与配置 JSON 来自同一次读取；构建 adapter 时 pin 该 revision。既有 synchronize 再读 SourceState，如果不匹配，在网络前返回 Conflict。网络期间修改、禁用、退休继续由 commit_snapshot/verify_unchanged 的最终 source CAS 拒绝，不能把旧配置配上新 revision 重放。已准入的外部 GET 无法跨数据库原子撤回；不把配置变更后已在途的请求误称“从未请求”。失败/超时/冲突不清空上次快照或推进成功核验。

只读 `Storage::source_authentication` 从既有 immutable credential.kind 选择 None/Basic/Bearer，其他类型/缺失引用 fail closed。调度器不读取密文/秘密；实际 SecretProvider、版本与 HTTPS/DNS/SSRF/重定向/字节限额继续由原 HttpFetcher 执行。conditional_read=false 清除传给 adapter 的验证器并拒绝304；启用时复用原配置绑定 checkpoint 与完整快照证明。自动快照使用 DeletionPolicy::Limit(100)，保留已有清空审批/删除阈值，不自动批准大批删除。

每次发现、配置读取、单源同步分别经过 Services::run_task；停止后无新准入，已准入的有界同步可保存结果，外层有限 drain 仍可取消。SourceProgress 只由真实查询/同步/外部结果产生；Scanned 和 Synchronized 携带当前缓存中是否仍有外部降级，单源成功不会清除另一来源的失败。存储故障返回上层，外部失败持续隔离重试；本模块不退出/重启进程，不操作 Docker，不解除恢复 pause/cutoff。

## 验证与未交付

10项集成测试使用真实 SQLite、真实配置 CRUD、测试专用 RemoteFetcher 到本地模拟 HTTP、注入 UTC/单调时钟和信号 barrier。覆盖周期/304/无条件读取、失败保留/独立退避、重启和未来核验时间、101来源分页、attempt/cache/单调预算、旧 record 网络前拒绝、在途修改/退休、停止排空、HTTP超时与非秘密 credential kind。实际 HttpFetcher 的 TLS/SSRF/认证矩阵在 workspace 原有测试回归；没有生产网络或 loopback 放行开关。

受控去掉 revision pin 与周期准入门槛，分别触发旧配置错误通过及提前轮询的行为断言失败；恢复后通过。夹具遗漏必需 DTSTAMP、退休保留旧核验时间的测试预期和语法错误不计作需求 Red。未运行长时 fuzz/形式证明/真实上游互操作。

标准 binary 尚未注册扫描器；生产 composition 等 #71 健康基础、认证 owner 的 admin/secret bootstrap 以及配置 owner 后续规则/output/freshness port。此模块不以空配置循环宣称首版已可用。

参考：[已有协调器与网络安全边界](source-coordinator.md)、[来源配置契约](source-configuration.md)、[Tokio watch](https://docs.rs/tokio/1.54.1/tokio/sync/watch/struct.Receiver.html) 的订阅/取消安全语义。
