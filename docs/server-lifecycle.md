# 服务装配与生命周期（#27）

`calendar-server` 依赖 application、adapters 与 storage；`Services` 持有迁移后的真实 SQLite 存储、应用投递策略和 ICS 接入预算。当前只安装健康、版本和 OpenAPI 只读路由；业务路由、生产任务调度和输出适配器由各自 Issue 交付，不启动虚假的投递器或连接外部服务。

## 启动

```sh
CALRELAY_BIND=127.0.0.1:8080 CALRELAY_DATABASE=/data/calrelay.db cargo run -p calendar-server --locked
```

必须显式指定监听地址与持久化数据库文件，数据库父目录必须已存在。先校验配置，注册 SIGTERM 处理器，再通过 `Storage::open` 完成迁移，最后绑定监听器；配置、迁移或绑定失败使进程非零退出。不存在“迁移失败但健康检查成功”的降级空服务。错误诊断不回显配置值或数据库内部错误。

HTTP 配置（均为启动时校验，非法数字不回退默认值）：

| 环境变量 | 默认 | 范围 |
| --- | --- | --- |
| `CALRELAY_MAX_REQUEST_BYTES` | 1048576 | 1–67108864 |
| `CALRELAY_MAX_RESPONSE_BYTES` | 1048576 | 512–67108864 |
| `CALRELAY_REQUEST_TIMEOUT_MS` | 30000 | 1–120000 |

原生依赖沿用存储的 bundled SQLite 及 adapters 的 Rustls/ring；不引入 OpenSSL 或运行时 SQLite 动态库要求。新增 Axum 固定为 0.8.6，锁文件保留已有依赖版本，仅增加 HTTP 服务端与信号处理需要的依赖/feature。

## HTTP 契约

- `GET /health/live` 返回 `{"status":"live"}`；`GET /health/ready` 在迁移完成后返回 `{"status":"ready"}`，停止时返回 503。
- `GET /api/v1/version` 返回 `name`、包 `version`、`api_version: "v1"`；`GET /api/v1/openapi.json` 返回 OpenAPI 3.1 文档及统一错误 schema。
- 每个请求由服务生成 `X-Request-Id`，不信任客户端提供的同名头；下游通过 `RequestId` extension 读取。失败响应形如 `{"error":{"code":"not_found","request_id":"…"}}`，正文 ID 与响应头一致，不包含底层异常细节。替换错误正文时保留 `Allow`、`WWW-Authenticate`、`Retry-After` 等协议头，移除旧正文长度、编码和校验缓存元数据。
- 全局边界在路由匹配之外运行，涵盖 404/405。读取请求正文时按实际字节计数，超限返回 413，不能靠缺失或伪造 Content-Length 绕过。响应先有界收集，超限整体替换为 500 `response_too_large`，不发送截断成功内容；错误 envelope 小于最小响应预算。当前边界面向有限 JSON 响应，未来流式路由需单独定义契约。
- 请求正文读取、处理和响应收集共用总时限，超时返回 408 `request_timeout`。路由不得阻塞 Tokio 执行线程。当前没有任何管理或摄取写路由。
- 健康端点按部署网络限制在内部，不在这里引入尚未交付的管理鉴权接口。

## 停止

SIGTERM/Ctrl-C 或 `Services::stop` 将 ready 置为 false，并触发 Axum graceful shutdown，停止接受连接。所有未来的任务领取循环必须通过 `Services::run_task` 进入应用操作；停止后拒绝新的任务准入。已经准入的有界操作持有 permit，服务等待其短事务提交/回滚，再关闭 SQLite 连接池。网络 I/O 仍遵循 application 的事务外执行约定。不得将无界后台循环放进 permit，亦不得绕过该门直接启动领取循环。

此 Issue 没有配置生产 worker 或真实 Bark。生命周期测试用本地合成事务证明停止后不再准入、在途事务能提交并重开数据库；进程测试向合成服务进程发送真实 SIGTERM，验证退出、监听关闭及 SQLite 完整性。

## AC 与验证

| AC | 自动验证 |
| --- | --- |
| AC1 | `http_contract_limits_ids_errors_and_openapi` 检查 ready 时迁移历史；`migration_failure_never_constructs_ready_service` 拒绝损坏 checksum；进程测试拒绝非数据库文件且保留原始文件 |
| AC2 | HTTP 集成测试覆盖版本、路径、404/405、实际正文限额、ID/错误、OpenAPI；边界单元测试覆盖精确响应预算、超限与总超时 |
| AC3 | `shutdown_blocks_new_claims_and_drains_committing_transaction` 真实 SQLite 与服务 graceful shutdown；进程测试覆盖 SIGTERM、缺失配置和迁移错误不监听 |

限额回归采用受控源码 mutation：临时取消请求字节上限，HTTP 契约测试必须从 413 退化为 200 而失败，再恢复生产实现验证 Green；编译失败不算 Red。实际执行结果见 PR。

实际参考：[Axum middleware](https://docs.rs/axum/0.8.6/axum/middleware/index.html)、[graceful shutdown](https://docs.rs/axum/0.8.6/axum/serve/struct.WithGracefulShutdown.html)、[有界 to_bytes](https://docs.rs/axum/0.8.6/axum/body/fn.to_bytes.html)。
