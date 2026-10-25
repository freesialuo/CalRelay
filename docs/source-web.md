# 来源管理前端（#89 / #77 第二阶段）

来源页面复用唯一配置/OpenAPI契约，读取分页摘要与详情，创建为禁用配置，编辑后显式启用/禁用，退休永久不可复活。ICS URL、ICS 手动配置、标准日历手动和 push-only producer 使用同一生成类型；本页面不上传文件、不抓取事件、不执行 producer，不提供规则/Bark/订阅配置或远端双向编辑。

## 登录与配置旅程

页面先读取公开、无授权能力的 `/api/admin/v1/bootstrap`，只接受已声明的 enabled / Turnstile site_key、hostname、action。关闭、暂不可用、错误结构或 hostname 不匹配均显示明确状态，不生成写入授权。用户点击验证后才从固定 Cloudflare 官方地址加载显式 widget；不启用隐藏 token 字段、自动失败重试或自动过期刷新。挑战完成回调只通过内存提交同源 POST session。挑战失败/过期/取消、登录拒绝与连接丢失允许显式重新验证，没有 token 粘贴或生产模拟登录。

CSRF/nonce/expiry 只在私有内存字段；HttpOnly Secure Cookie 由浏览器按同源策略发送。刷新后只读回服务端状态，重新验证才能取得写入 grant；页面离开销毁 widget、取消请求并清除授权，过期主动禁用写入。退出调用真实 DELETE session，只有204才报告确认；结果未知时清除本地授权并明确说明服务端未确认。公开 sitekey 不等于权限，source/feed bearer 不提供管理身份。

`SourcesAPI` 从生成声明引用配置/记录/摘要/页类型，验证有界响应、ETag 与 config revision。创建仅使用 If-None-Match: *，更新/退休仅使用 quoted If-Match；来源 revision 不作为配置写条件。服务端业务/能力校验为权威。日历既有 ID/external_id 只读，取消选择保留映射；新映射使用新稳定 ID，至少保留一个且最多256个；不可用的添加/移除动作明确禁用。

来源 GET 需要管理身份/会话；未装配或拒绝不会显示为空实例。来源保存成功仅表示配置提交，不能证明已抓取、规划、发送或设备显示。能力描述源侧承诺，不冒充 runtime 已执行；static/manual 无周期与 SLA，push-only 无周期且 SLA 可选。离线不是核心服务故障。

## 写入竞争与失败

`AdminSession` 全局阻止并发管理写入，发送前消耗本地 nonce；接到新的 X-Admin-Write-Nonce 才恢复后续写入。授权业务失败也读取轮换；缺失/相同 nonce、403、网络失败或取消不复用旧授权。并发登录拒绝重复派发，旧页面或已取消的回调不能恢复已清除的授权。凭据不存 DOM/storage/URL，不输出日志或分析。

- CAS 冲突保留草稿并阻止覆盖；读取新版本只展示当前状态，必须显式采用才替换草稿与写条件。
- 取消写入不等于撤回。丢失响应为结果未知，不自动重试；重新验证与刷新列表不会静默抹去编辑器中的未知提示，需显式读回核实。
- 重复点击不能再发送写入。返回/离开需确认放弃未保存输入；刷新不重放写请求。
- 退休有二次确认，服务端204后禁用写入。历史 record 可读，同一 ID 不能重建或复活；已发往外部的请求无法跨系统原子撤回。

## 构建与运行验证

需要 Node.js 24、Rust、Python3、OpenSSL 与 Chromium。Cloud 新 shell 先 `source /workspace/.calrelay-tools/env.sh`，随后在仓库目录运行：

```sh
cargo build -p calendar-server --locked
cd web
npm ci --ignore-scripts
npm run check:generated
npm run lint
npm run typecheck
npm run build
npm run check:bundle
npx playwright install chromium
npm test
```

已有系统浏览器时可使用 `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/chromium npm test`。`test:smoke` 跑实际生产构建的基础界面/本地只读HTTP与内存会话测试；`test:components` 使用单独测试入口和 route fixtures 验证界面故障/竞争；`test:integration` 显式编译 test-harness feature 的独立 calendar-admin-harness，运行生产构建与真实 guard / HTTP / SQLite。三套通过现有 frontend job 的 npm test 接入 quality-gate，不新增或放宽共享门禁。

真实集成代理只接收 harness 的 loopback 上游与 synthetic_only 合成身份；自签 TLS 只用于本地测试，忽略证书错误仅限测试浏览器上下文，不改变系统信任或网络设置。代理注入合成 Access JWT，绝不改写 Origin；后端继续实际验签、校验 session/CSRF/nonce、条件写入与 SQLite 事务。JWT只在代理内存，临时状态文件仅保存本地地址/DB路径。退出停止子进程并清理临时库/证书/状态。标准 binary 没有模拟登录开关，测试辅助文件不进入 production build。

widget 仅在测试拦截官方脚本请求，返回每次唯一的合成挑战；其他非本地网络请求全部拒绝，不接触真实 Cloudflare、ICS 或 Bark。CSRF/Origin 安全负例通过真实 HTTP transport 注入错误头并转交真实403，核对 DB revision 不变；Chromium 自己控制的 Origin 不能用前端 fetch 伪造，也不通过代理“修正”。直接 bearer-only 请求验证不能替代管理身份，未使用真实来源/feed token。

## 证据与边界

- 基础/会话：nonce轮换/业务失败/响应丢失/并发登录与写入/授权到期通知；刷新、取消、恶意文本与错误响应。
- 组件：空列表→禁用创建→启用→编辑→退休→刷新；CAS草稿/显式采用；403/409/422；未知结果/返回/晚到详情隔离；static/push字段隐藏、最少映射动作禁用、移动表单。
- 真实 HTTPS + guard + SQLite：创建/启用/编辑/禁用/退休与刷新回读、同ID重建拒绝；双浏览器CAS；CSRF/Origin无副作用；取消已提交响应但仅一次提交/显式回读/重新登录不丢未知提示；注销；重复挑战回调；挑战过期/失败显式重试；移动布局。
- 有效 Red：403错误分类、并发登录派发2/预期1、static周期字段可见、最少映射移除动作可点击；真实长ID移动标题溢出，换行后通过。受控去掉 widget 回调去重后真实登录回归失败，恢复后通过。Origin故障注入夹具修正不算产品Red。
- bundle canary 检查覆盖全部应用TS，并排除测试挑战/离线widget标记；没有source map、危险HTML渲染、storage、应用日志或构建env读取。不是对任意未来代码的完整安全证明。

生产 Access/Turnstile账户、真实Siteverify、部署TLS/静态资源托管、ICS抓取、生产工作器、首次凭据初始化、规则/输出/订阅页面和产品全旅程不在本批验收。已有 credential_id 只是引用，不能当作凭据初始化完成。#77/#35保持开放。

依据：[Turnstile显式渲染](https://developers.cloudflare.com/turnstile/get-started/client-side-rendering/)、[widget回调/字段配置](https://developers.cloudflare.com/turnstile/get-started/client-side-rendering/widget-configurations/)、[官方CSP要求](https://developers.cloudflare.com/turnstile/reference/content-security-policy/)、[Node HTTPS](https://nodejs.org/api/https.html)。CSP仅增加固定官方脚本/iframe来源，不启用 unsafe-inline/eval；原基础证据见[管理前端](admin-web.md)。
