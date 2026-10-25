# 首版系统验收计划与外部夹具（#113 / #78）

本页是可执行夹具与待接通验收契约，不是 #78 完成证明。基线 main `e5c5ac3` 已含 #109 的真实配置 UI；#107 runtime 分支参考 `f734f5e`，尚未作为已合并依赖使用。#110/#111 镜像与 CD 的最终提交/镜像 digest 由父任务提供。#78 AC1–AC7、#35/#77/#76 残余 AC 全部保留。

## 最小系统路径

保留现有74项前端/会话/真实guard SQLite回归，不再复制逐页CRUD矩阵。后续只增加一组串联场景，在同一受控库中依次验收；复用已有登录/widget/配置操作辅助函数，仅在实际接入时提取需要的部分。

| 步骤 | 真实动作及必须观察的证据 | #78映射 | 当前证据 |
| --- | --- | --- | --- |
| 1 | 空临时DB、正式CLI从合成secret文件初始化handle；真实HTTPS浏览器登录，配置两个ICS来源、稳定日历、输出、规则、聚合订阅并显式启用。A支持条件读取、B不用条件读取，均支持snapshot；周期遵守真实schema下限，不缩短持久配置伪造快速刷新 | AC1/6 | 单资源/跨页配置已由#109验证；串联双源runtime未接通 |
| 2 | 实际周期scanner获取两份同裸UID的ICS，持久原子revision；实际planner创建水位/jobs；实际delivery经当前output/generation与加密provider构造请求，模拟Bark返回accepted，attempt实际入库。聚合GET得到两条不同稳定导出UID，条件GET得304 | AC1/4 | UNVERIFIED，等正式runtime测试装配 |
| 3 | A返回503，B仍更新，核心live正常且无来源失败引起的核心故障；A恢复后304保持可信snapshot。改期后UID不变、取消状态保留，记录source revision和规划水位 | AC2/3/4 | 夹具具备条件/故障/改期/取消协议，系统结果UNVERIFIED |
| 4 | 屏障持有旧fetch响应，浏览器修改来源配置或永久退休，再释放：旧配置结果不能提交/复活任务。持有Bark响应时停用输出/规则再接受：保存真实accepted与竞争disposition，不声称撤回已发出的外部效果 | AC2 | 夹具具备屏障；系统generation/fence未接通 |
| 5 | 普通订阅编辑保留token、改变字段/选择使旧ETag失效；轮换/退休及来源失权时，旧token/ETag不能得到200/304或旧payload。URL只在内存/测试请求中，不写报告/日志 | AC2/4 | #109已有真实guard/SQLite配置/轮换/退休；带事件和缓存竞争UNVERIFIED |
| 6 | 保持同一受控DB/key重启实际runtime，accepted逻辑投递不重发；停止后用既有一致性备份/恢复流程得到pause/cutoff，重启和推进时钟均不自动resume/改cutoff/补发。显式恢复授权后的旧cutoff抑制需单独观察 | AC1/3/7 | 现有storage/runtime模块测试保留；浏览器串联未接通 |

AC3 的DST、UTC前后跳、强杀与完整追赶窗口，AC5 的producer负例矩阵，AC7 的全部核心停滞/故障，及真实日历客户端更新/删除互操作不被上述最小路径豁免；先引用已有有效模块证据，剩余系统场景继续由#78跟踪，不用本夹具数量代替集成验收。

## runtime owner需要冻结的测试语义（待协调，不是新生产接口）

现有 `calendar-admin-harness` 仅装配真实guard/静态API，未注册三工作器且拒绝已有DB；不修改它的安全限制来冒充生产装配。#107 的 `with_production_runtime` 固定构造生产时钟/HttpFetcher/BarkOutput，网络模拟与可控时钟目前仅在模块内部 `cfg(test)` 测试中，外部浏览器程序不能调用。需要 owner 给出确切feature、binary/库入口和提交，再接系统测试：

- 仅显式测试 feature 的独立程序，loopback绑定、全新受控临时目录；复用同一scanner/planner/delivery循环和预算、supervisor、真实配置查询/SQLite原子事务/代际fence。不能复制工作器或用fixture回调直接写事件/jobs/attempt。
- 真实CloudflareVerifier使用合成JWT签名/JWKS/Siteverify transport；保持身份/session/Origin/CSRF/nonce校验。复用已有HTTPS测试代理/官方widget测试拦截，不增加生产mock登录。
- 与正式CLI使用同一合成KeyRing文件/数据库，runtime实际通过只读CredentialProvider解密handle，并在每次投递前查询当前job/output generation。网络替身只替代外部传输，不能跳过这些步骤或在测试PushOutput里直接返回Accepted冒充完整配置分发。
- 仅测试transport将三个固定logical HTTPS URL映射到本夹具loopback目标；拒绝其他目标。不能添加生产私网豁免、部署mock字段、环境后门，不能改系统DNS/CA或代理到真实Cloudflare/Bark。若该层不能复用真实适配器TLS/SSRF逻辑，须明确网络层是模拟，继续单独保留现有adapter安全/TLS响应矩阵。
- 受控Clock分别推进UTC和单调时间，等待由状态/屏障驱动且有总期限；不睡60秒来代替周期证明，不缩短API合法周期。鉴权时钟与runtime业务时钟边界需owner明确，避免推进业务时间意外伪造会话。
- 测试父进程管理同一新建目录内的停止/重开，复用既有备份/恢复API产生pause/cutoff。不得放宽admin harness的任意既存DB拒绝，不新增生产HTTP pause/resume或控制时钟路由。Node对DB只读观察，运维变更由既有可信Rust用例完成。
- 启动/控制/退出回执只含版本、loopback地址和状态；合成AccessJWT仅代理内存。stderr不含原始URL、secret、事件正文。所有子进程和屏障有清理期限，提供SIGTERM与强制清理的实际状态。

以上是请求的能力边界，没有在本叶子自行冻结参数/DTO或实现runtime hook。当前完整串联动作处于**待依赖/契约，不是权限拒绝，也未执行**。

## 生产镜像和harness的证据分界

| 载体 | 能验证的动作 | 不能据此声称 |
| --- | --- | --- |
| 最终标准生产镜像（不含test-harness），必须记录source SHA/digest/平台 | 标准启动要求admin/KeyRing并failclosed；内嵌网页/资产/CSP；缺身份/错误权限请求拒绝；正式credential CLI与加密持久化；真实工作器注册、空队列/私网目标被生产SSRF拒绝时HTTP/liveness仍工作；非root/只读secret/data持久卷、SIGTERM/重开、恢复暂停。仅对实际执行的场景记PASS | 离线环境未实际完成的正向Access/Turnstile登录、成功远端ICS/成功Bark投递、所有恢复/客户端行为。外部拒绝的attempt不算accepted；镜像存在不算runtime全旅程 |
| 显式测试feature独立harness + 真实Rust静态HTTP/guard/SQLite + 受控网络/时钟 | 浏览器配置、真实条件写/nonce、实际周期/规划/持久投递状态、同UID多源、fetch/send/cache竞争、accepted去重、受控重开/恢复pause；每个实际场景独立记录 | 生产镜像具有测试控制入口、生产SSRF/TLS已被成功网络替身覆盖、真实Cloudflare账号可用、真实设备显示、真实客户端自动刷新、Docker恢复策略已验证 |
| 当前Node夹具协议测试 | 真实loopback HTTP的ICS/ETag/304/503、改期/取消/屏障、Bark JSON响应/预算、脱敏journal/清理 | 服务实际运行了scanner/planner/delivery或完成#78 |

镜像#110现有smoke与CD#111已有结果只归属它们的精确head/image，不能替代包含#107/#109的最终组合。无正式digest时不写“生产镜像验收通过”；不为本叶子连接生产或发布registry。

## 当前可执行夹具

```sh
source /workspace/.calrelay-tools/env.sh
cd web
npm run test:fixtures
```

`tests/system/fixtures.mjs` 只有显式调用 `startSystemFixtures` 才监听127.0.0.1随机端口。输入须为安全整数UTC毫秒与 `synthetic_` 前缀的合成Bark值，无默认secret。返回 `targets`、有界元数据 `snapshot`、程序内控制 `source`/`bark`/`holdNext` 与幂等 `close`；没有远程控制端点，也不访问外部地址。

两份ICS共享 `same-system-uid`，A/B标题不同，默认在基准时间+120秒/+180秒；改期最多±7天，取消增加SEQUENCE，ETag按源revision改变。`holdNext`在响应快照捕获后等待，允许后续请求读取新版本；外部状态操作不写CalRelay数据库。HTTP正文64KiB、journal最近256条并累计overflow、最多32连接/每socket64请求，报头/完整请求3秒（100ms巡检）、独立body3秒总deadline、响应停滞5秒有界；持续来字节不延长请求期限，实际触发允许事件循环调度容差，close取消在途等待/连接。Bark检查device_key/title/body但仅保留kind/status/字节数/匹配布尔；不记录正文、key、请求路径或头，错误不回显输入。此最小模拟器只实现验收所需协议子集，不是完整ICS/HTTP/Bark兼容性证明。

10项Node内置测试通过，新增持续trickle的partial body/header分别约3秒关闭与显式undefined拒绝后ICS/ETag/正文保持不变的回归（先Red后Green）；受控将条件读取304改为200时，validator测试以200≠304失败，恢复后通过。它们在既有npm test中先执行；Playwright忽略system目录，避免把Node测试误当浏览器测试。生产bundle检查继续执行，夹具不被应用导入。

依据：[RFC5545](https://www.rfc-editor.org/rfc/rfc5545) 的内容行、UID/SEQUENCE/STATUS；[RFC9110](https://www.rfc-editor.org/rfc/rfc9110.html) 的条件读取/304语义。

## #116：已运行的首条成功链（主线#112组合）

#112已合入 main `0067635`，实际 `Server::with_test_runtime_ports(runner,output)` 仅在 test-harness feature 提供；不再把该入口缺失作为当前阻塞。上文矩阵第1/2步的最小harness成功链已本地运行：浏览器经真实HTTPS/HTTP CSP、合成签名验证guard/CSRF/Origin/nonce，使用正式CLI加密handle配置两路pull ICS、输出、规则及汇总订阅；真实IcsSourceRunner/共享scanner/planner/delivery循环在SQLite提交2个event、2个watermark、2次accepted和2个accepted logical，聚合返回两个不同导出UID且条件GET304。其余矩阵步骤没有由这一结果自动通过，生产镜像全链仍UNVERIFIED。

执行（先构建web/dist）：

```sh
cd web
npm run build
npm run test:system
```

npm test也包含该唯一串联场景，现有7个夹具协议+74个旧回归保留；browser helpers从原配置集成测试提取复用，没有再复制全套CRUD。系统测试默认120秒总期限，真实UTC将两条合成事件安排在夹具启动约60秒后，通过有界状态轮询等待，不固定sleep或缩短source配置周期。允许真实周期再次读取ICS，首次两源成功必须分别观察，不把合法304/再次200当失败。

仅现有feature-gated calendar-admin-harness新增 `--system-fixtures FILE --deployment-config FILE`，不修改生产main/配置schema/SSRF适配器。默认fresh仍拒绝已有DB；系统模式也只能创建新DB，部署绑定必须loopback随机端口、无生产admin字段，原合成签名Verifier随后装配。复用正式DeploymentConfig/KeyRing与CLI；固定logical HTTPS目标映射到受控HTTP夹具，未知目标、非loopback/重定向/错误认证拒绝。网络替身保留真实output_for_delivery查询、凭据provider解密、JSON响应code判断及有界body/timeout；实际BarkOutput的TLS/SSRF/完整payload协议仍由独立adapter证据覆盖，不能把此替身当生产网络适配器已成功发送。

共享Rust所有权：#116只维护 `src/bin/admin_harness.rs` 的显式system模式/原认证复用及新 `src/bin/admin_harness/runtime_ports.rs` 网络替身；#115维护shared runtime Clock与fresh/reopen/restored typed接口。#116不改production_runtime.rs、不复制loops、不接reopen/restore控制。夹具新增只读/status返回脱敏journal，不提供远程控制操作。

有效检查：受控从harness移除实际三worker装配后，浏览器配置仍真实保存，但系统检查在events/watermarks=0（预期2）处失败；恢复后成功。原合成Siteverify用响应生成时间作为challenge_ts，跨秒可能晚于login采样时间而被真实Verifier拒绝；新增确定性测试先Red，再将合成挑战完成时刻设为响应前30秒后Green，真实future timestamp验证不变。多选源测试改用实际combobox可访问角色的定位修正不计产品Red。

后续#115 Clock推进必须考虑aggregate_feed独立feed::now()和鉴权时钟；未来freshness时间导致503是正确拒绝，不能关闭校验。受控重开/restore pause/cutoff、跨竞争与最终镜像精确source SHA/digest均待对应正式入口/镜像后继续验收。#78/#35/#77/#76保持开放。
