# Bark JSON 输出（#33）

`calendar_adapters::bark::BarkOutput` 实现既有 `PushOutput`；每个实例绑定一个 output ID、一个 credential handle 和 HTTPS origin。调用者按 output ID 分派，错误 output ID 在取凭据前拒绝。每次发送从 `CredentialProvider` 读取 kind=`bark` 的原始 UTF-8 device key，支持凭据替换；不缓存 key，不改变公共凭据接口。

发送固定 `POST /push`，`Content-Type: application/json; charset=utf-8`。JSON 只含 `device_key`、事件 `title` 和 `body`（description，无则 title）。不执行事件内 URL、附件或收件人。origin 只接受根路径或 `/push`，拒绝 URL 用户信息、query、fragment 和其他路径，避免旧版 key-in-path。所有诊断只暴露固定配置错误或 `DeliveryOutcome`，不记录请求/响应正文、key 或底层错误。Debug 隐去配置与凭据。

生产传输只连公网 HTTPS：复用现有公网地址策略，校验全部 DNS 结果并固定连接地址、校验 TLS 和实际 peer；禁用环境代理、重定向、透明重试与压缩。私网自托管访问当前拒绝，不提供隐式绕过；测试 root/loopback 注入仅在 `cfg(test)` 下存在。origin 可指定自托管公网主机与端口。DNS 最多 64 个结果、响应最多 16 KiB、title/body 合计最多 64 KiB、key 最多 4096 ASCII 可见字节；总尝试超时可配置在 (0,120s]，应用层仍保留自己的 deadline。

| 可观察结果 | `DeliveryOutcome` |
| --- | --- |
| HTTP 2xx 且完整 JSON 整数 `code=200` | Accepted（服务接受，不代表设备显示） |
| HTTP 429/5xx，或 HTTP 2xx + 应用 429/5xx | Retryable |
| 其他 HTTP 非 2xx（含重定向）、HTTP 2xx + 应用其他 4xx | PermanentFailure |
| HTTP 2xx + 缺失/无效/不认识应用码，响应过大/截断/丢失或超时 | Unknown |
| 确定连接失败、DNS 失败、临时凭据存储错误 | Retryable |
| output 不匹配、不允许地址、无效 key/凭据拒绝或认证失败 | PermanentFailure |

非 2xx 状态在读正文前分类，因此不会因错误正文损坏而丢失 429/5xx。`Retry-After` 支持秒数和 HTTP-date，转成既有 `retry_after_ms`；过去日期为 0，大数饱和为 i64::MAX，非法值忽略。已收到的 Retry-After 在正文丢失或本地超时后仍保留。队列负责退避、截止与重试；适配器每次只发送一次，不改变队列算法。响应丢失后重试可能重复通知，不承诺端到端 exactly-once。

## 验证证据

| AC | 自动验证 |
| --- | --- |
| AC1 | 本地 TLS 模拟服务检查 POST 路径/JSON/转义、header 无 key、每次重新取句柄；拒绝 URL 凭据和 key 路径、私网先于取凭据拒绝、Debug/错误脱敏 |
| AC2 | HTTP/应用码组合矩阵、错误状态不依赖正文、无效/超大/截断 JSON、无响应超时、错误 output/凭据 |
| AC3 | Retry-After 秒数/日期/负数/无效/溢出；响应头后超时保留；模拟服务收到请求后断开，再由调用方重试并记录两次请求 |

有效 Red：分类初始桩返回 Unknown，HTTP 200 + 应用 200 的验收测试实际失败；实现后保留同一测试。网络测试仅连本地合成 TLS 服务，使用合成 key；不发送真实 Bark、不读取真实用户凭据。

核对的官方来源：[Bark API V2](https://github.com/Finb/bark-server/blob/master/docs/API_V2.md)、[route_push.go](https://github.com/Finb/bark-server/blob/master/route_push.go)（单设备 POST 与 HTTP/应用码）、[reqwest ClientBuilder](https://docs.rs/reqwest/0.12.24/reqwest/struct.ClientBuilder.html)（TLS、代理、重定向、重试与 DNS pin）。实现复用锁定依赖，没有 Cargo.toml/Cargo.lock 变化。
