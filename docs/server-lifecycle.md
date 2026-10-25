# 服务装配与生命周期（#27）

`calendar-server` 依赖 application、adapters 与 storage；`Services` 持有迁移后的真实 SQLite 存储、应用投递策略和 ICS 接入预算。当前安装健康、版本、OpenAPI 和 #30 机器路由；#34/#80 管理会话/配置只由显式可信装配启用，默认 binary `admin=None`。生产任务调度和输出适配器由各自 Issue 交付，不启动虚假的投递器或连接外部服务。

## 启动

监听配置由 #80/#82 的单一部署 JSON 契约定义。健康命令复用同一 bind 读取函数，不读取监听环境变量、不加载数据库；监督预算通过 typed API 配置，不新增环境变量绑定。

```sh
CALRELAY_DATABASE=/data/calrelay.db cargo run -p calendar-server --locked -- --deployment-config /path/to/deployment.json
```

deployment.json 只包含 `{"bind":"部署者明确选择的数值地址:端口"}`（此文字是格式占位，不是有效地址）；不提供默认监听或替代监听 env。旧 `CALRELAY_BIND`（包括空值）在读取文件/打开 DB 前拒绝，给出移除该项并使用 `--deployment-config FILE` 的升级诊断，不回显值。未知/重复字段、非法地址、缺参数或不可读文件均启动失败。必须显式指定监听地址与持久化数据库文件，数据库父目录必须已存在。先校验配置，注册 SIGTERM 处理器，再通过 `Storage::open` 完成迁移，最后绑定监听器；配置、迁移或绑定失败使进程非零退出。不存在“迁移失败但健康检查成功”的降级空服务。错误诊断不回显配置值或数据库内部错误。

HTTP 配置（均为启动时校验，非法数字不回退默认值）：

| 环境变量 | 默认 | 范围 |
| --- | --- | --- |
| `CALRELAY_MAX_REQUEST_BYTES` | 1048576 | 1–67108864 |
| `CALRELAY_MAX_RESPONSE_BYTES` | 1048576 | 512–67108864 |
| `CALRELAY_REQUEST_TIMEOUT_MS` | 30000 | 1–120000 |

原生依赖沿用存储的 bundled SQLite 及 adapters 的 Rustls/ring；不引入 OpenSSL 或运行时 SQLite 动态库要求。Axum 固定为 0.8.6；server 的 SQLx 测试依赖随 main 对齐为 0.9.0，保留主线升级及安全 SQL API 调整。锁文件在 main 基础上仅增加 HTTP 服务端与信号处理需要的依赖/feature，SQLite 引擎仍为 libsqlite3-sys 0.30.1。

## HTTP 契约

- `GET /health/live` 检查真实核心进展；`GET /health/ready` 检查迁移后装配、DB 读写和 worker 初始进展。标准入口尚无必需生产 worker，明确返回 503；成功响应形状仍为 `{"status":"live"}` / `{"status":"ready"}`。恢复暂停 live 可保持、ready=false，停止时 ready=false。见 [核心监督](core-supervision.md)。
- `GET /api/v1/version` 返回 `name`、包 `version`、`api_version: "v1"`；`GET /api/v1/openapi.json` 返回 OpenAPI 3.1 文档及统一错误 schema。
- 每个请求由服务生成 `X-Request-Id`，不信任客户端提供的同名头；下游通过 `RequestId` extension 读取。失败响应形如 `{"error":{"code":"not_found","request_id":"…"}}`，正文 ID 与响应头一致，不包含底层异常细节。替换错误正文时保留 `Allow`、`WWW-Authenticate`、`Retry-After` 等协议头，移除旧正文长度、编码和校验缓存元数据。
- 全局边界在路由匹配之外运行，涵盖 404/405。读取请求正文时按实际字节计数，超限返回 413，不能靠缺失或伪造 Content-Length 绕过。响应先有界收集，超限整体替换为 500 `response_too_large`，不发送截断成功内容；错误 envelope 小于最小响应预算。当前边界面向有限 JSON 响应，未来流式路由需单独定义契约。
- 请求正文读取、处理和响应收集共用总时限，超时返回 408 `request_timeout`。路由不得阻塞 Tokio 执行线程。机器写路由见 protected-ingest，管理配置见 [来源配置](source-configuration.md)。
- 健康端点按部署网络限制在内部，不在这里引入尚未交付的管理鉴权接口。

## 停止

SIGTERM/Ctrl-C 或 `Services::stop` 将健康就绪与业务准入置为 false，并触发 Axum graceful shutdown，停止接受连接。所有未来的任务领取循环必须通过 `Services::run_task` 进入应用操作；停止后拒绝新的任务准入。已经准入的有界操作持有 permit，服务等待其短事务提交/回滚，再关闭 SQLite 连接池。网络 I/O 仍遵循 application 的事务外执行约定。不得将无界后台循环放进 permit，亦不得绕过该门直接启动领取循环。

恢复数据库的持久 `recovery_control.paused` 和历史 cutoff 不由 server 启动或停止修改。HTTP ready 与 `Services::ready` 的业务准入分离，恢复暂停时健康 ready=false；恢复后的投递仍受存储 guard 约束，直到运维人员在隔离/停服状态下显式执行 resume，历史 cutoff 继续保留。

此 Issue 没有配置生产 worker 或真实 Bark。生命周期测试用本地合成事务证明停止后不再准入、在途事务能提交并重开数据库；进程测试向合成服务进程发送真实 SIGTERM，验证退出、监听关闭及 SQLite 完整性。

## AC 与验证

| AC | 自动验证 |
| --- | --- |
| AC1 | `http_contract_limits_ids_errors_and_openapi` 检查 ready 时迁移历史；`migration_failure_never_constructs_ready_service` 拒绝损坏 checksum；进程测试拒绝非数据库文件且保留原始文件；`fresh_server_applies_migration_0009_before_ready` 核验恢复迁移与默认策略 |
| AC2 | HTTP 集成测试覆盖版本、路径、404/405、实际正文限额、ID/错误、OpenAPI；边界单元测试覆盖精确响应预算、超限与总超时 |
| AC3 | `shutdown_blocks_new_claims_and_drains_committing_transaction` 真实 SQLite 与服务 graceful shutdown；进程测试覆盖 SIGTERM、缺失配置和迁移错误不监听；`server_restart_preserves_recovery_pause_until_explicit_resume` 核验实际备份恢复后经两次 server 启停仍暂停、无租约/尝试，显式 resume 后仅 cutoff 当时及之后的任务可领取 |

限额回归采用受控源码 mutation：临时取消请求字节上限，HTTP 契约测试必须从 413 退化为 200 而失败，再恢复生产实现验证 Green；编译失败不算 Red。恢复生命周期回归另经受控 mutation 验证：临时在启动中自动调用 resume，暂停断言实际从 `(1,1000)` 变为 `(0,1000)` 而失败；恢复原实现后通过。实际执行结果见 PR。

实际参考：[Axum middleware](https://docs.rs/axum/0.8.6/axum/middleware/index.html)、[graceful shutdown](https://docs.rs/axum/0.8.6/axum/serve/struct.WithGracefulShutdown.html)、[有界 to_bytes](https://docs.rs/axum/0.8.6/axum/body/fn.to_bytes.html)。
