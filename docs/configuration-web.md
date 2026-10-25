# 规则、Bark 输出与汇总订阅网页（#106）

本叶子延续[来源管理](source-web.md)，使用同一管理登录、内存 CSRF/一次 nonce 和生成 OpenAPI 类型。服务端唯一契约见[提醒配置](reminder-configuration.md)、[汇总订阅](aggregate-feed.md)。页面只提交配置，不执行通知测试、事件编辑、强制客户端刷新或运行时装配。

## 最小使用旅程

1. 从来源页创建来源、稳定日历映射与能力，保存后显式启用。静态来源不显示假轮询，来源离线不是核心全局故障。
2. 首次 Bark 凭据由部署管理员按[安全 CLI](credential-bootstrap.md)从受控 secret-file 初始化。标准命令为 `calendar-server credential provision-bark --deployment-config FILE --credential-id ID --secret-file FILE`，主密钥与 Bark secret 不进入浏览器。将成功回执中的 credential_id 填到“输出与订阅”页；网页仅引用已有 kind=bark 句柄，不要求手改数据库。缺失/不符凭据的真实422显示失败，不能保存为假成功。
3. 创建禁用 Bark 输出，核对 HTTPS origin（允许 /push）、句柄与毫秒超时，再显式启用。保存不发送通知；accepted 也不代表设备已显示。
4. 在“日历与规则”页读取真实来源选项，再读取所选来源日历并选择；读取输出并绑定。规则至少选择一个有效日历/输出；可以预先绑定禁用资源，实际执行仍需资源启用。提前量支持0.001秒，全天提醒时间使用 HH:MM[:SS]。创建先禁用，再显式启用，修改/停用/退休均使用 generation 条件写。
5. 在“汇总订阅”选择来源/日历与公开字段，设置 UTC ISO 8601 到期时间（含 Z；可空）。空选择就是空日历，不扩大为全来源；空公开字段仍保留必要 UID/时间/状态。首版陈旧策略固定 reject，任一所选来源失权或不可用时整份拒绝。同裸 UID 不跨来源合并。
6. 创建禁用订阅后，服务仅一次返回 capability。显式显示或复制并妥善保存，再启用；普通配置编辑保留原链接。显式轮换需要确认并立即使旧链接失效，需要管理员自行更新日历客户端。永久退休不可复活。到期链接不可用，已过期配置启用会被服务拒绝。

来源与输出选项支持有界分页；已有选中 ID 独立保留，不因选项读取失败而丢失。选项读取不会自动改变选择；后端保存时重新校验引用，退休/取消选择/并发变更可使保存失败。没有创建秘密管理 API 或第二套持久配置 DTO。

## 竞争、返回与隐私

新建发送 If-None-Match: *；规则/输出更新与退休使用 generation，订阅更新/轮换/退休使用 revision 的 quoted If-Match。授权业务失败也消费/轮换 nonce，页面不会并发发送写请求。CAS 保留草稿并阻止再次覆盖，需要读取、比较后显式采用服务端版本；普通刷新列表不替换编辑器输入。

请求发出后取消等待不是撤回；回执丢失显示结果未知、消耗本地授权并阻止重发。重新登录不会抹去编辑器中的未知提示。显式读取可确认配置，但 GET 不提供丢失的一次性链接；需核对版本后显式轮换。返回/跨页对未保存输入、进行中的请求或尚在本页的链接确认；刷新不重放请求。详情读取期间旧编辑器锁定，失败恢复原状态；晚到列表/详情不会覆盖当前选择。

链接默认不写入输入值，显式显示后可隐藏；可单独清除本页链接。仅内存保留2分钟，页面离开/会话失效时清除，GET/list/普通编辑不会重新展示。链接不成为可点击 href，不进入地址栏、history、storage、应用日志或分析。显示/复制是管理员主动泄露给屏幕/系统剪贴板的操作；复制仅在浏览器 promise 成功后报告，拒绝明确说明并允许手动复制，本页不会自动清除系统剪贴板。不会承诺浏览器或进程内存的安全擦除。

全部名称、日历标识及响应状态使用 textContent 或表单 value；无 HTML 注入。第三方请求仍仅用于固定官方 Turnstile widget。监听地址与部署安全设置不进入业务 UI。浏览器关闭/移动系统终止未必触发生命周期确认，不能保证恢复未保存输入；刷新需要重新取得内存写授权。

## 验证

沿用 `web` 中 lint、typecheck、build、check:generated、check:bundle 与 npm test。无需新依赖或 workflow 门禁改动。test:integration 显式编译标准 CLI 和 feature-isolated admin harness，仅在全新临时 SQLite 中使用安全 CLI 从合成 secret/key 文件初始化测试凭据；不直接插入凭据行、不弱化 guard，不修改真实凭据或系统信任。HTTPS 测试代理仅 loopback，官方 widget 脚本仅测试拦截，其余非本地网络拒绝；没有真实通知。

新增真实浏览器/HTTP/SQLite覆盖：输出/规则配置、生成版本/绑定与刷新，安全CLI引用、独立会话 generation CAS、订阅来源/日历选择、一次性显示/复制/复制失败、普通编辑保留token、轮换后旧token拒绝、新token可读、停用/退休、到期/空选择、CSRF/Origin拒绝且DB不变、取消已提交响应不重复写、显式回读/采用、恶意名称不执行与移动截图。保留来源完整回归。有效Red包括缺失订阅入口、取消已选日历后可选框仍勾选；测试代理缺少feed转发、CDP读取回执的夹具竞争修正不算产品Red。

#77/#35 保持开放：ICS上传/手动接入执行、来源/规划/投递业务诊断与审计、最终生产装配和跨服务结果仍待独立验收。#76 的真实日历客户端互操作/刷新与删除语义未验证；#78负责系统级真实runtime+模拟Bark验收。这里不把配置成功当作事件刷新、规划或设备显示完成。

依据：[Clipboard.writeText 的成功与拒绝](https://developer.mozilla.org/en-US/docs/Web/API/Clipboard/writeText)、[beforeunload 的移动端限制](https://developer.mozilla.org/en-US/docs/Web/API/Window/beforeunload_event)。
