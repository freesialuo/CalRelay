# 来源协调器（#29）

Stack 合并顺序：[#55 / #26](https://github.com/freesialuo/CalRelay/pull/55) → [#56 / #28](https://github.com/freesialuo/CalRelay/pull/56) → 本项 #29。分支串联派生，按现有 CI 约定各 PR 指向 main；父层合并前差异累计。父层尚未合并，不解除下游 Block，不提前关闭 Issue。

## 入口与原子边界

`calendar-application::synchronize` 读取 SourceRepository 状态，通过 SourceAdapter 逐页获取/解码，在内存收齐完整结果后只提交一次 FullSnapshot。网络和解析期间不持有数据库事务；本项不发送提醒或实现 OAuth2 刷新。

- 同源并发采用来源 revision CAS。两个基于同一 revision 的完整快照只有一个提交成功；迟到结果返回 Conflict，必须重新获取，不能把旧批次改成新 revision 重放。
- 第一页和后续页共用当前状态；只在所有页成功、显式结束、总预算满足且领域/批次校验通过时调用原子仓储。分页失败、重复游标、集合版本缺失/变化、第二页 304、重复 UID、无效事件、超限或超时均不提交部分结果。
- 默认至多 32 页、32 MiB 输入、10000 条记录、60 秒；配置上限 128 页/64 MiB/10000 条/120 秒。单页映射受 8 MiB 输入及累计映射字段预算约束。总超时包住获取与仓储调用，每页返回后、进入提交前也复核单调时钟，防止同步解析耗时越过期限后继续提交。超时不等于进程抢占式 CPU 限制；各解析器仍有自身大小/复杂度预算。
- `DeletionPolicy::Limit` 传给已有原子仓储；空的可信完整结果也受缺失删除阈值约束。事件、日志、checkpoint、revision、last_verified_at 与新增快照标记同事务写入。提交被取消/连接丢失的结果可能未知，恢复时重新读取来源状态，不盲目重放。

## 304 与真实快照证明

迁移 `0006` 增加 `sources.snapshot_revision`，只有成功的完整快照提交会设置；delta 不伪造快照证明。历史 checkpoint 可能来自 delta，因此 v5 升级保留旧数据但标记为空，先完整获取一次，再接受 304。新列限制为正值且不大于来源 revision。

304 只允许第一页且已有完整快照；`verify_unchanged` 条件更新核对 revision、enabled 和 snapshot_revision，只提高 last_verified_at，不改事件、日志、checkpoint 或 revision，较晚到达的旧核验时间不会降低水位。每次完整提交仍推进 CAS revision，即使内容未变也会使旧批次失效，但不重复生成事件历史。

HTTP 适配器将 URL、calendar_id、鉴权句柄/类型和映射配置摘要绑定到验证器 checkpoint，配置改变后不复用旧验证器。JSON 分页的首页 ETag 不能证明整份集合未变，所以只在单页完整文档上保留验证器；分页首页清除 ETag/Last-Modified，后续页不发条件请求。无可用验证器时不允许远端返回 304。这样牺牲分页条件请求优化，避免后续页变化被首页 304 隐藏。

## ICS 与受限 JSON 映射

`calendar-adapters::ingestion::HttpSource` 通过 #28 RemoteFetcher 获取文档。ICS 使用 #25 的有界 normalize_ics，再交给来源指定的 calendar_id；完整快照表示整个来源，不将单日历子集冒充多日历来源的完整集合。

JSON 配置：items 为页内数组的 JSON Pointer；fields 是 EventV1 顶层字段到 item 内 JSON Pointer 的映射；next 必须指向字符串下一页 URL或显式 null。分页响应另须提供顶层 snapshot_id（非空字符串、最多 256 字节），所有页保持一致；没有集合版本或版本中途变化即拒绝完整快照，不将动态变化中的分页拼成误删除快照。缺失 next 不等同结束。示例：

```json
{
  "items": "/items",
  "fields": {
    "external_uid": "/uid",
    "title": "/name",
    "status": "/status",
    "start": "/when"
  },
  "next": "/next"
}
```

映射固定注入 schema_version=1.0，仅允许明确列出的 EventV1 字段，不支持脚本、模板、类型转换、任意表达式或隐式默认事件身份；映射字段不存在或类型/领域非法则整次失败。指针长度最多 256，字段数受白名单约束，单页最多 10000 事件。next 先相对于返回它的当前页 URL 解析为绝对游标，再校验初始 HTTPS 同源，不能把凭据发到他源或注入 URL userinfo/fragment。

## 新鲜度与投递

SourceState::push_ready 提供 enabled/last_verified_at 的保守诊断；实际发送前仍由 #24 的存储投递检查读取最新来源状态、新鲜度期限和规划水位。禁用或超过配置 source_max_age_ms 的来源暂停推送；本项不在获取失败时清空旧事件或把失败记为成功核验，亦不绕过 #24 的最终投递复核。UTC 回跳时诊断不把未来 last_verified_at 当成当前新鲜度证明。

## AC 与实际验证

| AC | 可执行证据（calendar-storage/tests/ingestion.rs） | 层级 |
| --- | --- | --- |
| AC1 | 两页完整后单次提交；第二页失败保留事件/版本/checkpoint；并发 CAS 只有一胜；重复内容不重复历史；旧 revision 拒绝；触发器注入来源写失败回滚快照标记和事件 | 真实 SQLite/并发/故障注入 |
| AC2 | 首次 304 拒绝；完整提交后 304 只提高核验时间；较旧核验不倒退；第二页 304、缺结束标记及重复 UID 拒绝；v5 升级不把旧 checkpoint 当快照 | 迁移/协议集成 |
| AC3 | 禁用拒绝启动同步；新鲜度边界和陈旧诊断；实际派发的禁用/陈旧复核复用 #24 既有回归 | 规则/已有投递集成 |
| AC4 | 模拟 RemoteFetcher → 真实 ICS/JSON 解码 → 协调器 → 原子文件库；跨配置验证器不复用，分页验证器不发送到后续页 | 集成；#28 另提供真实 TLS 传输模拟 |

Red：SourceRepository 占位实现下，两个协调验收测试分别以 Unavailable 与预期 304 分类不符失败；实现后通过，扩充至 7 项协调集成测试。受控把缺失分页结束标记改成 None，端到端测试捕获 `Ok(Committed(...changed_events:2))` 而预期 InvalidSnapshot 的误删除；恢复生产实现。未运行长时 fuzz/模型检查或真实外部来源。

PR #57 review 回归：三页链 `/api/events` → `/v2/pages/2` → `3` 在旧实现中实际请求 `/api/3`，与预期 `/v2/pages/3` 的 URL 断言失败；修复为当前页相对解析后通过。另验证跨源、HTTP 降级、userinfo 和 fragment 下一页均在提交前拒绝且保留来源状态。

## 实现参考

Exa 核对 [SQLx 0.8.6 公共源码](https://docs.rs/crate/sqlx/0.8.6/source/README.md) 和 [SQLite TransactionManager](https://docs.rs/sqlx/latest/sqlx/sqlite/struct.SqliteTransactionManager.html)，复用 #22 的 BEGIN IMMEDIATE、条件 UPDATE 和回滚边界；固定版本行为由真实文件库、故障注入与升级测试核对。JSON Pointer 采用 serde_json 的受限数据选择 API，不解释代码。
