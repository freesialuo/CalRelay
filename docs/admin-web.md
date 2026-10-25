# TypeScript 管理入口基础（#81 / #77 第一阶段）

此阶段提供可运行的页面、构建/类型生成/浏览器测试，以及实际只读 HTTP 检查；不是 #77/#35 的完整配置旅程。标准 binary 已支持显式部署文件装配管理 verifier；启用 `production-web` feature 时由 Rust 在同源 `/admin/` 提供真实构建静态资源。默认开发构建不携带页面。完整生产循环、容器打包和部署验收仍未完成，不能把开发 preview 当生产服务。

## 运行

需要 Node.js 24、仓库 Rust 工具链与 Chromium。Cloud 新 shell 先执行 `source /workspace/.calrelay-tools/env.sh`；该路径仅是保存环境工具初始化约定，不是产品配置。

```sh
cargo build -p calendar-server --locked
cd web
npm ci --ignore-scripts
npm run generate
npm run lint
npm run typecheck
npm run build
npm run check:bundle
npx playwright install chromium
npm test
npm run dev:local
```

`dev:local` 在 `http://127.0.0.1:4173/admin/` 提供已构建页面，启动独立 Rust 子进程、临时 SQLite 文件和回环临时端口。仅代理同源 `/api` 与 `/health` 至这个子进程，不连接已有实例或生产服务；退出会停止子进程、删除临时库。harness 为这个临时实例写入部署 JSON，通过 `--deployment-config` 显式选择回环临时端口，不再传入已移除的 `CALRELAY_BIND`；JSON 随临时库一同清理，不作为产品默认监听配置，页面不处理监听设置。`npm run dev` 仅提供静态开发页面，没有默认模拟或生产代理。

已有系统 Chromium 时可执行 `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/chromium npm test`；CI 使用 Playwright 固定版本对应的 Chromium。测试针对生产构建。route fixtures 仅存在于 `web/tests`，无生产 mock 开关。

## 类型与构建边界

`npm run generate` 启动上述本地真实服务，读取其 `/api/v1/openapi.json`，用锁定 openapi-typescript 生成 `web/src/generated/api.d.ts`。`check:generated` 做逐字比较，源头是实际 Rust OpenAPI，不复制配置 DTO 或提交另一个 JSON 权威。Rust 改动后先重建服务，再更新生成类型。声明包含现有可选管理会话、来源配置/分页发现与机器 API，并不意味着默认 binary 装配了这些路由，也不会赋予前端机器管理权限。

Vite + TypeScript，无运行时 UI 框架；固定 npm lockfile，所有工具是开发依赖。只通过固定同源 GET 路径读取，拒绝重定向；检查有 8 秒预算和 4 KiB JSON 上限，不自动重试。请求中重复点击被禁止；取消、返回和刷新不复用旧结果。成功只描述本次 HTTP 证据，不证明来源、工作器或配置写入成功。鉴权 401/403、404 未接通、409 冲突、限流及 503 均有明确状态。响应正文错误不直接渲染；request ID 限长并筛选字符。版本等不可信文字通过 textContent 渲染。

应用不接受或保存管理/source/feed token，不使用 localStorage/sessionStorage、分析或应用日志，不读取构建环境变量。Cookie 只由浏览器按同源策略发送。bundle 检查以合成环境密钥 canary 重建并扫描产物，同时防止 source map 与应用源码危险 HTML/storage/log/env surface；这是回归检查，不是对任意未来代码的完整安全证明。页面 CSP 限制同源脚本/连接，生产仍须以安全 HTTP headers 和可信管理装配保护。

## 旅程与待接契约

页面按来源 → 能力与周期 → 日历与规则 → 输出与订阅 → 诊断组织。缺失接口以说明呈现，没有空保存按钮或伪成功：

- 来源列表未知不当作空实例；ICS 的修改表示 URL/文件替换，不向远端写事件。
- 静态/手动源不显示轮询；push-only 仅在明确 SLA 时判断 freshness。
- 接入器离线不等于核心故障；现有 HTTP live/ready 不能证明生产工作器注册或运行。
- Bark accepted 不等于设备显示；结果未知不能触发自动重复发送。

#73/#80 唯一配置 owner 已预告 GET/PUT/DELETE `/api/admin/v1/sources/{source_id}`、创建 `If-None-Match: *`、更新/退休 `If-Match` revision；尚未合并，不接到当前页面。完整空实例旅程仍需列表/发现、日历映射、能力/周期、规则/Bark/订阅 DTO、nonce 轮换、CAS 冲突/结果未知核实、一次性密钥返回和业务诊断/审计契约。没有这些接口不能完成创建/修改/删除、重启回读和在途失效验收。source/feed token 拒绝管理 CRUD、Origin/CSRF/nonce 与真实管理登录也需后续 HTTP/SQLite 集成；本阶段只验证前端拒绝状态呈现，不声称实现服务端安全检查。

## 验证映射

| #81 AC | 证据 |
| --- | --- |
| AC1 | lockfile、Biome、tsc、Vite，frontend job 纳入原 quality-gate；保留 contracts/pr-policy/rust |
| AC2 | 真实服务 OpenAPI 漂移检查；浏览器真实版本/live/可选 session 404；刷新/重复点击/取消/返回/错误响应测试 |
| AC3 | 旅程导航、未接通说明、无保存与假数据；静态/推送源能力说明 |
| AC4 | Chromium 恶意文本渲染、只读方法与 storage 检查、bundle canary；服务端权限负例待配置集成 |

受控 bundle mutation：临时开启 VITE_ 暴露并在应用读取合成 VITE_SECRET，canary 检查明确失败；恢复后通过。

浏览器缓存恢复回归：派发 persisted pageshow 后旧成功证据仍保留（Red）；恢复事件重新渲染并取消旧检查后 Green。该测试检查恢复生命周期 handler，非浏览器缓存命中率证明。

TDD：可运行占位入口下，浏览器在找不到“从来源开始”旅程标题处失败；补页面与导航后通过。网络、重定向、HTML 登录页、格式错误、超大响应、401/403/404/409/429/503 使用仅测试 route fixtures。实际版本/健康与 session 未装配用临时库真实服务，不能以 fixture 通过声称业务持久化完成。

依据：[Vite env 暴露边界](https://vite.dev/guide/env-and-mode)、[openapi-typescript Node API](https://openapi-ts.dev/node)、[Playwright 浏览器](https://playwright.dev/docs/browsers)与[测试配置](https://playwright.dev/docs/test-configuration)。setup-node v6 的完整 SHA 从官方仓库 tag 解析固定；不改变现有 Action 或权限。

## Rust 同源生产静态托管（#102）

```sh
(cd web && npm ci --ignore-scripts && npm run build && npm run check:bundle)
cargo build --release --locked -p calendar-server --bin calendar-server --features production-web
```

先构建 `web/dist`，再编译 Rust。feature 缺少产物时明确失败，默认构建不需要 Node/dist。构建脚本只纳入首页及 Vite 哈希命名 JS/CSS，拒绝非普通文件和超过默认 1 MiB 响应预算的单份资源，不纳入 source map、环境文件或其他目录。构建工作区必须可信且干净；allowlist 不是对恶意构建产物的扫描替代。锁定依赖与 bundle canary 仍须通过。`--bin calendar-server --features production-web` 不启用 `test-harness`，不会产生管理测试程序。运行镜像无需 Node 或 web/dist。

显式 GET/HEAD `/admin/`、`/admin/index.html` 及实际 `/admin/assets/<hashed-name>.js|css`；无 SPA 全局 fallback、无运行时任意文件读取。未知页面/资产、API/health/feed 保留既有 404 JSON 边界，其他方法返回 405。首页 `Cache-Control: no-store`，哈希资产一年 immutable；更新首页选择新哈希。资源使用明确 MIME、nosniff、no-referrer、DENY 和 CSP；CSP 仅额外允许固定 Turnstile 官方脚本/frame 域，不允许任意外链、内联脚本或 eval。HTML 自身 CSP 与 HTTP CSP 同时生效，页面所需资源须同时满足两者；实际认证页面与 #94 合并后继续组合验收。

静态资源本身不授予管理身份。生产入口仍必须在可信代理/Access 边界保护整个管理 host 与 `/admin/`，管理 API 持续验证 JWT/session/CSRF/Origin/nonce；静态服务不会加载 secret 或绕过这些 guard。监听仍只有部署 JSON，未新增 env、字段或默认端口。反向代理不得覆盖安全头或把未知 API 改写为首页。实际 TCP HTTP 测试覆盖页面/资产、HEAD、类型/缓存、安全头、编码 traversal 负例和 API 共存；这是静态托管证据，不代表 UI 写入或业务循环已验收。
