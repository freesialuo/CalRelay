# 管理身份与应用会话（#34）

## 范围与装配

`calendar-adapters::admin_auth` 验证 Cloudflare；`calendar-storage::admin_sessions` 独立持久化浏览器状态；`calendar-server::admin` 提供会话 router 和未来管理写操作的 `protect` guard。不读取、更改 API/feed token 表，也不把管理中间件挂到 ingest/feed。无管理 UI，无真实账号配置或外部验证调用。

默认二进制不挂载管理路由（404）。可信装配代码显式构造 `AdminConfig`、`CloudflareVerifier::new(config, SecretBytes)`，然后 `Server::with_admin(Arc::new(verifier))`。配置接口要求 team slug、应用 audience、完整 HTTPS origin、Turnstile hostname/action、明确的 `subject => verified email` 管理员列表及 1–900 秒 session TTL；无默认管理员或共享口令。secret 从部署者的可信 secret provider 注入；本功能不接收、保存或配置真实 Cloudflare secret。`with_transport` 和可注入时钟仅为可信装配/离线测试接口，不能由请求选择。未来配置入口须保持相同验证，并遵循项目凭据加载边界。

## 验证与失败行为

- 每次请求都独立验签 `Cf-Access-Jwt-Assertion`，不使用身份声明头、Forwarded/X-Forwarded-*、客户端 Cookie 中的 Access token 或未验签 claims 授权。只接受 RS256，严格解析 JWT header，按唯一 `kid` 匹配 RSA 签名 JWK；拒绝 token 自带远端 key URL、重复 kid、未知算法和缺失声明。
- issuer 固定为配置 team 的 `https://<team>.cloudflareaccess.com`，aud 必须包含配置的应用 audience，type 必须为 app。必须有非空 sub、allowlist 中对应的精确 email，且 `iat/nbf <= now < exp`。无宽限；拒绝服务 token、组织 token 和已到期 token。JWT 最大 16 KiB。
- 每次验签从固定官方 `/cdn-cgi/access/certs` 取得 JWKS，覆盖轮换时的两个签名 key；无过期 key 缓存或失败回退。HTTP 仅 HTTPS、验证 TLS、不读环境代理、不跟重定向；连接最多 3 秒、完整调用最多 5 秒、响应最多 64 KiB。上层注入 transport 也受 5 秒和 64 KiB 限制。
- 登录调用固定官方 Siteverify，secret/response 在 POST form 内；不传入来自伪造头的 remoteip。必须 success、hostname/action 精确匹配且 `0 <= now - challenge_ts < 300` 秒。空值、超 2048 字符、解析失败、超时、错误响应全部拒绝。外部调用不持有数据库事务；调用后重新核对时间。
- 伪造 Access 头、绕过代理直接打源站、只有管理 cookie 或只有一个有效 JWT 均不能取得管理写权限。持有完整有效 JWT、应用 session、CSRF 和未消费 nonce 的管理员可以直接通过相同应用验证；本模块不配置或证明源站网络隔离。

## HTTP 契约

OpenAPI `/api/v1/openapi.json` 描述可选的 `/api/admin/v1/session`：

| 方法 | 必需条件 | 结果 |
| --- | --- | --- |
| POST | Access JWT、精确 Origin、JSON `turnstile_token` | 200，`__Host-calrelay_admin` cookie；body 含 csrf_token、write_nonce、expires_at（Unix 秒） |
| GET | Access JWT、绑定的 session cookie | 204，确认当次身份与会话有效 |
| DELETE | Access JWT、session、Origin、X-CSRF-Token、X-Admin-Write-Nonce | 原子撤销并消费 nonce；204、清空 cookie |

Cookie 使用 Secure、HttpOnly、SameSite=Strict、Path=/，不设置 Domain。所有会话路由响应 `Cache-Control: no-store`。登录请求体最多 4096 字节，仍受 server 的全局大小/超时/错误 envelope 边界约束。重复的身份、Origin、CSRF、nonce header 或同名 session cookie 拒绝。Origin 缺失、null、异源或 Sec-Fetch-Site 为跨站均拒绝写操作。CSRF token 和 nonce 只返回浏览器一次，应保存在内存；不要保存到 URL 或日志。

未来管理 handler 必须由 `admin::protect` 包装。每个非 GET/HEAD 方法在 handler 前原子比较 session、identity、policy、CSRF、nonce、期限和 revoked_at，成功才运行 handler，并把新的 nonce 放入 `X-Admin-Write-Nonce`。同一 nonce 的并发请求只有一个可运行 handler。失败/取消不会恢复已消费 nonce；若没有拿到新 nonce，重新完成登录。该一次性授权不是业务幂等性保证，敏感业务提交还须在其最终事务中处理配置变化和撤销竞争，不能缓存长期授权结果。

拒绝为固定 403；server drain 为 503；外部/SQL 错误和输入 token 不进入响应。Json/大小/方法等协议错误由现有边界处理。日志不得输出 JWT、挑战、cookie、CSRF、nonce、secret 或表内管理员身份。

## 持久化与兼容

迁移 `0010_admin_sessions.sql` 新增 admin_sessions/admin_challenge_uses，与预留 0007/0008 无关；不修改 0001–0006/0009 内容或 #30/#32 token 语义。会话、CSRF、nonce、挑战只存带用途前缀的 SHA-256 摘要；随机浏览器值来自 OS CSPRNG（各 32 字节）。管理员绑定（issuer/sub/email）为普通数据库身份数据，备份因此包含该信息；不含原始外部 JWT、secret 或挑战。

session 最晚于初始 Access JWT 到期或 session TTL 到期失效。配置策略摘要覆盖 team/audience/origin/hostname/action/管理员列表/TTL，变更即使旧会话失效；撤销和 nonce 消费重开数据库后保持有效。挑战摘要至少保留至本次验证后 300 秒，与 session TTL 无关，阻止模拟提供方重复成功以及并发登录。过期行在下一次成功创建会话的事务中清理。UTC 时钟来自可信服务时钟，时间回退到创建前拒绝；不提供任意系统时钟回拨或特权数据库历史回滚下的防重放保证。恢复历史备份也恢复当时的会话状态；恢复到可访问服务前应更换管理策略/撤销会话，使备份中的授权失效。

真实 v9 历史库通过 SQLx 正常升级，不重写旧 checksum；v9 与 v10 的备份均通过现有 manifest/migration/checksum 校验并在隔离目录恢复。0010 失败会整体回滚，不破坏 v9 数据/历史。未来合并 0007/0008 后由父任务执行组合迁移、服务和备份测试。

## AC 与验证

| AC | 实际测试 | 层级 |
| --- | --- | --- |
| AC1 | adapters/tests/admin_auth.rs：合成 RSA 签名、claims 缺失/错误、身份、算法、kid、伪造签名、Siteverify hostname/action/时效与错误响应 | 协议/安全负例 |
| AC1/3 | adapter 私有本地 TCP 模拟：HTTPS-only、302 不跟随、503、响应超限；无真实外部调用 | 传输集成/故障注入 |
| AC2/3 | server/tests/admin_auth.rs：完整登录、cookie/身份绑定、Origin/CSRF、重复头、并发 nonce、注销/到期、服务故障、默认路由关闭和错误脱敏 | HTTP 集成/验收 |
| AC2 | storage/tests/admin_sessions.rs：持久撤销/重放、跨身份/策略拒绝、并发挑战与写入、插入故障不烧挑战 | SQLite 并发/故障注入 |
| 兼容 | storage/tests/admin_migration.rs：v9→v10、旧 checksum 不变、历史及当前备份恢复、失败迁移回滚 | 迁移/恢复集成 |

有效 Red：占位会话实现返回 Denied，生命周期测试在预期成功创建会话处失败（编译和夹具正常），实现后通过。额外受控 Origin gate mutation 的结果及最终完整验证记录在 PR。

## 官方依据

- [Cloudflare Access JWT 验证、JWKS 与 key rotation](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/validating-json/)
- [Access application token 类型与身份字段](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/application-token/)
- [Turnstile Siteverify：单次、五分钟、hostname/action](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/)
- [ring 0.17.14 RSA 公钥验证](https://docs.rs/ring/0.17.14/ring/signature/struct.RsaPublicKeyComponents.html)

通过 Exa 检索并读取以上官方资料；未自行实现 RSA。测试 fixture 明确标记公开合成测试 key，只用于本地签发伪造测试 JWT，绝不能用于部署。无形式证明、长时 fuzz 或真实 Cloudflare/生产环境验证声明。
