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

### main 集成（#30 已合并）

普通 merge 同步 main `653b4c6` 后，保留 #30 的 ingest router、Services policy、Schema validator、OpenAPI describe 和0007；管理路由仍单独可选挂载。旧 v9 夹具明确只装1–6、9，升级允许补入较低编号0007，再应用0010；逐条核对旧版本checksum，不再假设非连续历史是新历史的连续前缀。完整本地 Rust 241项通过；最终CI/Codex以PR当前head记录为准。#32组合验证在独立临时工作树进行，不把其未合并代码写入本分支。

## 标准 binary 的部署装配（#93）

`--deployment-config FILE` 仍是监听地址唯一入口，`CALRELAY_BIND` 继续拒绝。部署 JSON 只追加可选 `admin`；不含该字段时保持关闭，`GET /api/admin/v1/bootstrap` 返回 `{"enabled":false}`。配置 admin 后，标准 binary 在打开数据库和监听之前验证并加载 secret、构造既有 Cloudflare verifier，再调用同一管理装配。缺失/错误字段不回退关闭，直接启动失败。旧 `Config` 解析入口遇到 admin 会显式拒绝，生产调用使用 `DeploymentConfig`，避免配置被静默丢弃。

以下全部是合成示例，不是账号或密钥：

```json
{
  "bind": "127.0.0.1:8080",
  "admin": {
    "team": "synthetic-team",
    "audience": "synthetic-audience",
    "origin": "https://admin.example.test",
    "hostname": "admin.example.test",
    "action": "admin-login",
    "site_key": "synthetic-site-key",
    "administrators": {"synthetic-subject": "admin@example.test"},
    "session_seconds": 300,
    "turnstile_secret_file": "/run/secrets/turnstile"
  }
}
```

所有 admin 字段必填；未知/重复字段、重复 subject、空身份列表、非 HTTPS 或 hostname 不匹配的 origin、无效 action/site_key、TTL 非1–900秒或相对 secret 路径均拒绝。部署文档最多64KiB，管理员最多64项。site_key仅含1–256个ASCII字母数字/下划线/连字符；action最多32个相同字符。绑定地址和已有数据库/预算环境配置不改为业务配置；不从环境变量读取 admin secret。

`TurnstileFileSecretProvider` 实现既有 `CredentialProvider`，仅响应固定 turnstile 用途，以只读方式打开绝对路径常规文件，读取最多4097字节后要求1–4096个ASCII graphic字节（不接受末尾换行）。Unix用O_NOFOLLOW拒绝末端symlink，用O_NONBLOCK避免FIFO阻塞，打开后检查常规文件；不会创建、写入或更改文件权限。部署者负责可信目录、只读secret挂载和文件访问权限，本实现不宣称验证挂载属性或所有父目录无symlink。错误仅为固定文本，无路径/身份/secret。没有新凭据表或迁移，不自动导入凭据。

公开 `GET /api/admin/v1/bootstrap` 始终 no-store。启用且 site配置完整时，仅返回：

```json
{"enabled":true,"turnstile":{"site_key":"synthetic-site-key","hostname":"admin.example.test","action":"admin-login"}}
```

不返回team/issuer、audience、管理员、origin允许列表、secret或路径；调用不验证或返回身份、不建立会话，也不授予CRUD权限。仅使用旧 `with_admin` 而没有site_key的内部装配仍可供已有测试使用，但公开bootstrap报告关闭以避免UI宣称可登录；生产使用完整装配。登录仍为POST /api/admin/v1/session，所有原JWT/Siteverify、Origin/CSRF、nonce、过期/撤销/重放规则保留。生产反向代理负责HTTPS与Cloudflare Access header传递；不在此配置账户或代理。

### 给 #89 的离线浏览器入口

显式构建才包含独立测试程序；标准 binary 没有mock配置字段、环境开关或测试登录路由：

```sh
cargo build -p calendar-server --features test-harness --bin calendar-admin-harness
./target/debug/calendar-admin-harness --database /tmp/new-isolated-test.sqlite --browser-origin https://localhost:4443
```

数据库路径必须绝对且不存在，程序独占创建文件、只绑定127.0.0.1随机端口；origin只允许HTTPS localhost或127.0.0.1。启动stdout第一行JSON包含 `upstream`、`browser_origin`、`access_jwt`、`challenge_prefix`、`synthetic_only:true`。这些是公开合成fixture；不要把该stdout或程序用于生产。Access JWT的audience/subject固定合成值，有效一小时；会话最多900秒。只接受 `synthetic-challenge:` 前缀、长度21–127的挑战，数据库仍原子拒绝重复挑战。

前端测试可在其HTTPS本地代理后运行：浏览器请求保持实际Origin，代理只向本地upstream注入上述合成Cf-Access-Jwt-Assertion，绝不重写Origin以绕过guard。浏览器侧仅在测试中模拟Turnstile widget返回新的合成挑战；随后POST登录，接收真实HttpOnly Secure cookie与内存CSRF/nonce。未来来源CRUD使用生成DTO和真实API，页面刷新从同一SQLite回读。harness无预建管理员会话、无预置业务行、无认证绕过。TLS终止/前端widget模拟由#89测试owner接入，不将HTTP开发预览冒充生产登录。

测试：新增标准binary子进程验证真实启用/公开元数据/伪造拒绝/坏secret启动失败；独立harness子进程验证签名后的错误身份和到期、伪造挑战、Origin/CSRF、登录、来源PUT/列表、nonce重放、到期/撤销及数据库重开。原#34全套负例保持。有效Red是bootstrap关闭状态原先404而预期200；来源API夹具修正不算Red。

实现查阅 [Rust OpenOptionsExt 官方文档](https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html) 与 [Linux open(2)](https://man7.org/linux/man-pages/man2/open.2.html)，保留原Cloudflare/ring依据。不宣称真实账号登录、实际外部Siteverify调用或首版完整产品闭环；#75/#77其余AC仍未由本叶子覆盖。
