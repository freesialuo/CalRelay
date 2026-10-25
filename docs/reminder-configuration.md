# 持久提醒规则与 Bark 输出（#92）

本批是 #73 / #35 的可独立验收叶子；保留父项全部 AC，订阅/多源聚合、envelope执行及生产安全/工作器装配不在本批。配置 owner 维护 application DTO、schema、0012、管理 OpenAPI 和生成 TS；runtime/UI 复用，不另造规则/事件/任务模型。部署 JSON/admin/secret provider 装配由独立 owner 负责，本批不新增这些入口，不请求真实 token 或配置第三方账户。

## 共享契约

`RuleConfigurationV1 {version,name,enabled,policy: RulePolicy,output_ids}` 直接复用现有 RulePolicy(calendar_ids,lead_ms,all_day_time)。1–256个不同日历、1–32个不同输出、提前0–7天，全天时间沿用 HH:MM[:SS]。引用必须是已托管且未退休的来源日历/输出；可预先保存指向 disabled 资源，运行发现只返回实际启用组合。绑定集合按id排序，旧绑定以 enabled=0 保存历史外键，不物理删除。

`BarkOutputConfigurationV1 {version,name,enabled,kind:bark,origin,credential_id,request_timeout_ms}` 只支持现有 BarkOutput。origin 沿用 HTTPS origin 或 /push、无 userinfo/query/fragment 的约束；只保存已有 kind=bark 的 credential handle。没有 raw device key/token 字段或凭据创建 API。GET/列表/错误/审计不读取明文或密文；handle不是密钥。实际取secret沿用 CredentialProvider/SecurityStore，只在受信任 adapter 边界暴露。origin 保存不是网络授权，Bark adapter 的 DNS/SSRF/HTTPS 防护仍在实际调用时执行。

request_timeout_ms 为1–120000（现有adapter范围），实际投递仍受外层 DeliveryPolicy.request_timeout_ms 与lease预算约束，runtime应取更小预算。business配置不从env读取，不重定义部署安全上限。名称/id/创建disabled/条件头沿用来源配置的规则。

## 管理 HTTP

| 路由 | 行为 |
| --- | --- |
| GET `/api/admin/v1/rules?after=ID`、`/outputs?after=ID` | 最多100条 ReminderConfigurationSummary；包含disabled/退休状态，无policy/credential数据，next_after续页，非跨并发快照 |
| GET `/api/admin/v1/rules/{id}`、`/outputs/{id}` | RuleConfigurationRecord / BarkOutputConfigurationRecord；ETag是现有generation |
| PUT 同路径 | 严格v1 schema；创建 If-None-Match:* 且 enabled=false；更新 If-Match:"N"；200/201返回本次提交receipt，不以提交后重读冒充自己的结果 |
| DELETE 同路径 | 空正文、If-Match:"N"；软退休并禁用，204；禁止同id复活/覆盖未知legacy |

复用 #34 Access/session/Origin/CSRF/一次nonce，不挂到 ingest/feed 身份。默认binary仍admin=None，只有可信装配后才提供这些路由。授权unsafe请求即使业务失败也消耗nonce，读取 X-Admin-Write-Nonce后重试。no-store；64KiB正文限制，保留登录4KiB；Content-Type为application/json。未知/重复字段、非法语义/引用/条件422，媒体类型415，CAS/退休/legacy冲突409，失败不回显原始配置/secret。版本化JSON schema在schemas，示例在examples；同一export_openapi命令和前端generate脚本产生统一声明，没有第二份手写TS模型。

## 持久化与失效

唯一新增0012_reminder_configuration.sql。main已有0011；#70保留0008；核对#71/#88/#91无新增迁移。本批不占其他任务文件/部署装配所有权。现有 reminder_rules、outputs、rule_outputs 继续是真相；managed=0 legacy保留原JSON/generation/启停状态，不猜测导入。测试seed现在显式列出 rule_outputs(rule_id,output_id)，新增enabled默认1不会破坏旧库历史；未合并runtime测试也需使用显式列名。

规则/输出写在BEGIN IMMEDIATE内验证引用与CAS，更新generation、审计和派生查询关系。rule_calendars是既有policy日历集合的事务索引，使runtime无需全表JSON扫描。rule_outputs不删历史绑定；planner和delivery复核enabled。修改规则/绑定或输出使旧pending/retry_wait/leased superseded，停用/退休使其cancelled；保留lease/attempt，以便已经发送的结果返回时如实记录accepted与竞争disposition。accepted任务/逻辑去重不重放，重新规划只建立未accepted的新generation。

现有 SecurityStore.replace_credential / rotate_credential 的revision CAS会触发同事务输出generation递增与任务失效；重加密也保守失效。credential_changed审计标记系统actor(null)，不复制credential内容。切换credential_id同样是配置CAS。审计/引用/迁移失败全部回滚，包括密码替换及其派生generation，不留下半次轮换。退休与accepted历史不自动清理。

## runtime port 与实际消费者

application `ReminderConfigurationRepository` 提供：

- `planning_configurations(after)`：至多100个 PlanningConfiguration，稳定(source_id,rule_id,output_id)游标；同一SQL快照返回source_revision/source_config_revision、rule_generation、output_generation、source_mode/freshness_sla_ms。只包含托管、启用、未退休资源和selected日历/有效绑定；不限于有新事件或当前新鲜来源，不因间歇离线丢失发现。循环下一轮从头扫描，不把游标分页当跨并发快照；改变后旧fence由既有reconcile事务拒绝。
- `output_for_delivery(job_id,output_id)`：在构造adapter前立即查询当前BarkOutputConfigurationRecord。要求当前job处于leased、已有对应lease_token的未完成sent intent、事件/规则/输出generation与绑定仍有效；失效返回None。不能把管理GET或缓存的输出记录替代这个port，不能用单一BarkOutput发送所有output_id。查询后仍有不可原子撤回的网络竞争，finish保留真实结果。

测试消费者通过发现fence构造现有PlanningRequest、调用真实reconcile，再让现有deliver_one驱动基于此port的mock dispatcher和现有加密provider；不是只有新trait/文档。生产循环由#75复用该接口，BarkOutput实际发送沿用现有adapter；本批没有周期循环、真实Bark网络或密钥装配。

freshness：发现不以last_verified_at过滤，明确返回SourceMode与显式SLA。manual-static无轮询SLA；push-only仅有显式SLA时判断陈旧；pull沿用其SLA。投递claim候选与prepare均按此契约核对：manual或无SLA push不套隐含24h；显式SLA需可信核验时间在当前时刻及SLA窗口内，legacy未托管来源保留既有DeliveryPolicy行为。#74/#75仍负责实际刷新、诊断与生产系统装配；不得把来源离线当全局健康故障。创建配置本身不会发通知。

验证使用真实HTTP+SQLite、mock secrets/发送、CAS/故障/在途接受、v11升级与备份恢复、OpenAPI/schema/生成类型、现有前端门禁。来源/订阅/生产安全装配的原验收仍由各自叶子负责；本PR不关闭#73/#35。

实际参考：[SQLite事务](https://sqlite.org/lang_transaction.html)、[AFTER trigger](https://sqlite.org/lang_createtrigger.html)、[历史外键](https://sqlite.org/foreignkeys.html)。
