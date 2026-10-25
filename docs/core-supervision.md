# 核心监督契约提案与当前边界（#36）

本分支先提供独立 healthcheck CLI。以下监督契约是待装配边界确认的最小设计，**尚未实现核心监督**；现有 main 的静态 live 与仅停止标志的 ready 不能作为 #36 验收证据。不得据此宣称服务已具备完整生产就绪探针。

## 实际组件与缺口

main 653b4c6 的 `Server::initialize` 校验配置、执行 `Storage::open` 迁移，`serve` 装配 HTTP/ingest、停止信号和任务准入/排空。它没有启动规划、调度/投递或来源扫描循环。

- `ReminderPlanner::reconcile(PlanningRequest, Clock)` 是单次应用操作；尚无生产扫描选择规则/输出组合、构造 PlanningRequest、延展水位与公平轮询的 server 接口。
- `deliver_one` 与 `DeliveryRepository` 是单次投递操作；尚无生产 PushOutput/Bark 装配、凭据读取与有界调度循环。不能新增伪造成功输出或将循环定时器当作投递进展。
- 来源 `synchronize` 已有应用契约，尚无生产来源枚举/调度装配。来源远端错误必须属于功能降级，而非核心死亡。

这些归属须由父任务协调。#69 管理认证和 #70 feed 尚未合并，不把它们当作工作器接口，也不在本分支改其行为。无需新迁移。

## 建议的最小监督契约

1. 每个实际装配的核心任务登记明确身份及有界单轮操作。完成真实扫描/空队列检查、事务提交或可识别的外部失败后才能更新进度；独立常驻 ticker 无权为它更新心跳。监督器必须观察任务返回、panic 与进度停滞。
2. live 检查 HTTP 事件循环与已装配核心任务的单调时间进展；配置化启动宽限仅容忍初始未进展，不能把 ready 置真。完全卡死由外部监督发现，进程不能承诺自救。
3. ready 需要配置有效、迁移完成、真实限频/短超时 DB 读写探测成功、全部必需工作器已装配并完成初始轮次。缺失装配必须显式不就绪，不能通过假工作器掩盖；缺失装配不应本身造成无限退出/重启。
4. 外部 ICS/Bark/Cloudflare 不可达、限流或超时是功能降级；工作器仍须遵守单轮预算并报告进展。它们不改变 live，不触发进程退出。不可恢复核心故障则停止准入，停止领取，等待有界在途操作提交/回滚，关闭池，以非零状态退出；正常信号退出为零。
5. `tokio::time::Instant`/Duration 承载宽限、停滞与关闭预算，不能用 UTC 差值决定重启。确定性测试用可注入单调时间/暂停时间推进；数据库 cutoff 和业务时间继续遵守原有 UTC 契约。
6. 恢复库 `recovery_control.paused=1` 时，工作器必须有真实的暂停检查进展而不领取/发送任务；live 可保持成功，ready 应明确不就绪（恢复需要运维核实），不能以重启清除暂停。任何监督、健康写探测或重开都不能修改 paused/cutoff 或复活历史任务。显式离线 resume 后 cutoff 继续有效。

ready 的新含义还应与未来写路由准入对齐；当前 `Services::ready` 同时用于 `run_task`，不应未经协调将恢复暂停混入它而阻断已有恢复回归的人工操作路径。监督状态与业务准入需要明确接口，而非大规模重构公共 HTTP 模块。

## 已实现的 CLI

```sh
CALRELAY_BIND=127.0.0.1:8080 target/debug/calendar-server healthcheck
CALRELAY_BIND=127.0.0.1:8080 target/debug/calendar-server healthcheck ready
```

默认检查 `/health/live`，显式 `live` 等价；`ready` 检查 `/health/ready`。只接受数值 loopback SocketAddr；通配监听地址转换成同地址族 loopback，零端口和非本机地址拒绝。不读取数据库配置、不打开/迁移数据库、不启动服务或工作器。当前二进制名沿用 `calendar-server`，制品命名由发布任务处理。

客户端不使用代理、不跟随重定向，仅 HTTP 200 响应头视为成功；不验证/输出响应正文，避免暴露详情。非 200、协议错误、连接失败或超时返回非零。`CALRELAY_HEALTHCHECK_TIMEOUT_MS` 默认 2000，合法范围 1–120000，作用于单次完整请求直到响应头；由 HTTP 客户端的单调计时实现。不依赖 curl、无新增依赖版本升级（复用锁定 reqwest 0.12.24）。未知命令/参数失败，不能误启动服务。

## 验证与未完成验收

CLI 有效 Red：新增真实子进程回归，执行 healthcheck 因错误进入服务启动、缺少 CALRELAY_DATABASE 而失败。Green：CLI 不依赖数据库配置，默认/显式 live 与 ready 路径、503/重定向/畸形/截断响应拒绝、总超时均通过本机临时 TCP fixture 验证；真实 server 进程同时接受 CLI 探测后 SIGTERM 正常关闭、数据库可重开。既有持久暂停/cutoff 回归原样保留。

AC1 核心心跳/DB 探针/启动宽限/未装配工作器就绪矩阵，AC2 核心失败非零/外部降级，AC3 心跳停滞，以及暂停下新 live/ready 语义仍待实现和验证。完整 #36 不可合并关闭；本分支不把 CLI 的成功冒充这些验收结果。

设计参考通过 Exa 查阅 Tokio 官方 API：[Instant 单调时间](https://docs.rs/tokio/latest/tokio/time/struct.Instant.html)、[pause/advance 的测试时钟](https://docs.rs/tokio/latest/tokio/time/fn.pause.html)、[JoinSet 的任务完成与取消](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html)。本次 CLI 复用现有 reqwest，不引入自制 HTTP 解析器。尚未实现/运行上述监督故障注入和暂停时钟测试。
