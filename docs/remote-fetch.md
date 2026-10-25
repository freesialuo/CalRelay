# 安全远程获取（#28）

Stack：父层 [#55 / #26](https://github.com/freesialuo/CalRelay/pull/55) 提供凭据句柄；本项提供 `RemoteFetcher` 应用契约与 `HttpFetcher`，下游 #29 负责解析、分页协调和数据库提交。本层不提交数据库，不执行 OAuth2 或任意 JSON 脚本。为适配现有 main-only CI，Stack 各 PR 指向 main，必须按父层到子层顺序合并；父层未合并不代表主线已有能力。

## 安全与资源边界

- 只接收 HTTPS；拒绝 URL userinfo、fragment、无主机和超过 8192 字节的 URL。响应作为受限字节返回，供 ICS 或 JSON 解码，不自动声称数据完整。
- 每一跳解析 DNS，并拒绝空答案、超过 64 条答案或任一非允许地址。IPv4 拒绝私网、环回、共享地址、链路本地/元数据、文档/测试、组播和保留地址；IPv6 仅接受原生 global unicast，并排除特殊分配、文档及转换机制，包括 IPv4-mapped、NAT64、Teredo、6to4。策略偏保守，不提供来源级私网例外。
- 每跳新建客户端，将通过检查的地址传给 `resolve_to_addrs`，保留原始主机名做 TLS 验证；无连接池复用、自动重定向或内部自动重试。实际 remote_addr 仍须匹配固定地址。第二次 DNS 查询不能发生在检查与连接之间。
- 应用客户端显式不采用环境代理：代理再次解析主机会破坏 DNS 固定保证。仅支持直接连接的部署网络；受管代理部署需另行交付可验证的出口解析策略。此规则属于产品传输实现，不修改开发环境代理或网络策略。
- 只允许同源（scheme/host/port 相同）重定向，每一跳重新解析并校验；跨源、HTTPS 降级、非法 Location、缺失 Location 或超过跳数显式失败，避免转发凭据和条件头。
- 默认 8 MiB、5 次重定向、30 秒总期限，可配置上限分别为 64 MiB、10 次、120 秒。总期限覆盖凭据获取、DNS、所有跳和完整响应流。Content-Length 与逐 chunk 累计均限额；只允许 identity 编码，拒绝压缩响应，防止压缩大小歧义。不接受 206 局部响应作为完整日历。
- ETag/Last-Modified 输入与输出各限 4096 字节；header 解析拒绝换行注入。只返回所需验证器，不保留任意响应正文诊断。

## 鉴权与失败分类

`RemoteAuthentication` 支持 None、Basic、Bearer，配置只保存 credential_id。通过父层 `CredentialProvider` 按 kind `basic`/`bearer` 在调用时取值。Basic 凭据是 JSON 对象 `{ "username": "…", "password": "…" }`；支持转义字符，拒绝 username 冒号及控制字符。Bearer 是非空可打印 ASCII token，最长 4096 字节。解码及编码临时缓冲使用 Zeroizing，Authorization 标记 sensitive。

不记录 URL、Authorization、原始凭据、响应正文或底层 reqwest/JSON 错误；错误为固定分类。请求/结果没有派生包含 URL/正文的 Debug。只在当次请求持有秘密，调用方仍须遵守不打印、不长久复制秘密的边界。

| 结果 | 行为 |
| --- | --- |
| 200 | 完整、未超限流读取后返回 Modified 和验证器 |
| 304 | 仅 trusted caller 声明已有已提交快照时返回 NotModified；否则 UnexpectedNotModified |
| 401/403 | Unauthorized，不混同网络错误 |
| 429 | RateLimited，保留 Retry-After |
| 5xx | Server，包含数值状态及 Retry-After |
| 其他（含 206） | Status，显式拒绝 |
| DNS/超时/连接或截断/大小/协议 | 各自类型化错误，不产生部分文档 |

Retry-After 支持非负秒数和 HTTP 日期，按注入的 now_ms 转为毫秒；过去日期返回 0，溢出/无效值返回 None。304 不推进 checkpoint 或自行更新数据库；下游需合并缺省验证器并绑定真实快照状态，不能用任意缓存存在冒充已提交快照。

## 验收与验证

| AC | 生产行为对应验证 | 层级 |
| --- | --- | --- |
| AC1 | 临时证书的真实本地 TLS 服务验证 None/Basic/Bearer、条件头、验证器和字节；Content-Length、chunked、截断及压缩拒绝；挂起 body 和 DNS 的总超时 | 协议集成/故障注入 |
| AC2 | 公私网/特殊地址表，整数/十六进制 IPv4 与 mapped IPv6 URL 拒绝；混合 DNS 答案、重定向重新解析私网拒绝；固定地址连接只查询一次 DNS；跨源、降级、错误 TLS 信任/主机名及重定向预算拒绝 | 安全负例/真实 TLS |
| AC3 | 首次/已有快照 304、401、429、503、404、206、Retry-After 秒数与日期、固定错误脱敏 | 协议集成 |

Red：先运行占位地址与状态实现，两项规则测试均以有效断言失败；Green 后扩充为 12 项规则/TLS 测试。受控把地址策略替换为总允许，拒绝表在 `0.0.0.0` 断言处失败，恢复生产实现后通过。沙箱首次阻止本地 bind 属环境失败，不作为业务 Red；允许本地网络后全部 TLS 测试通过。测试的 loopback 放行与自签根注入仅在 cfg(test) 编译，生产构造器不暴露这些开关。未访问真实外部来源或使用真实凭据，未运行长时 fuzz/形式证明。

## 实现参考

Exa 查询 [Reqwest ClientBuilder](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html)、[redirect::Policy](https://docs.rs/reqwest/latest/reqwest/redirect/struct.Policy.html) 及 [公共客户端源码](https://github.com/seanmonstar/reqwest/blob/master/src/blocking/client.rs)，再核对实际下载并锁定的 `reqwest 0.12.24` 的 `src/async_impl/client.rs`、`src/retry.rs`：`resolve_to_addrs`、no_proxy、Policy::none、retry::never、TLS 及响应 chunk API。参考资料的 latest 不作为版本证据，实际编译及本地 TLS 测试核对固定版本行为。
