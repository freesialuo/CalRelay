# ICS 订阅渲染档位 v1（#31）

`calendar_adapters::renderer::render_ics(&[FeedEvent])` 是纯函数，返回完整 VCALENDAR 或类型化错误，不读取数据库、当前时间或网络，不展开重复实例。调用方应在一致快照中提供事件元数据；订阅 token、路由、缓存、server 生命周期及快照查询不在本模块范围。

## 身份与修订

- `event_id` 必须是全局唯一、删除/恢复/来源重命名时不变的内部事件 ID。UID 固定为其 UTF-8 SHA-256 小写十六进制摘要加 `@calrelay`，与标题、时间、裸上游 UID 无关。空或重复内部 ID 整个 feed 拒绝。
- `revision` 为正整数内部事件修订，SEQUENCE = revision - 1；超过 RFC INTEGER 上限 2147483647 显式拒绝，不能截断或回绕。上游 source_sequence 不参与映射。
- `changed_at` 为该内部修订的 UTC 秒精度时间（YYYY-MM-DDTHH:MM:SSZ），映射 DTSTAMP。重复渲染不能传当前请求时间；修改、删除或恢复必须传对应修订时间。上游 source_modified_at 不参与映射。
- `deleted=true` 使用最后保存的 document 输出同 UID、递增 SEQUENCE 的 STATUS:CANCELLED 母事件墓碑。文档自身为 cancelled 时同样输出取消。取消整个系列时不输出独立例外，以免活动例外复活。墓碑保留/过期策略由上层负责，本模块不会因删除而遗漏记录。

## 已验证支持与拒绝

| 内容 | 支持范围 |
| --- | --- |
| 时间 | DATE（排他 DTEND）、UTC DATE-TIME、floating DATE-TIME、带 TZID 的本地 DATE-TIME；不转换为 UTC。年份 0001–9999，秒精度。DTEND / DURATION 保留互斥及日历 duration 语义 |
| 正文 | SUMMARY、DESCRIPTION、LOCATION、URL、CATEGORIES、STATUS、CLASS、TRANSP；可选空数组与缺省视为相同日历语义 |
| 重复集合 | 已通过领域校验的 RRULE 原样保留；RDATE（含 PERIOD 的独立 end / duration）、EXDATE；无无限展开 |
| 例外 | range=single 且 change=cancelled，输出同 UID、同修订、原始 RECURRENCE-ID 与 STATUS:CANCELLED 的 VEVENT。override 与 THISANDFUTURE 均显式拒绝，不能降为单次或丢失 patch |
| 时区 | zoned 值须提供与 TZID 匹配的源 VTIMEZONE。用既有受限 ICS 归一化器验证组件，再输出规范组件；相同定义去重，同名不同定义整份 feed 拒绝。无定义的 IANA TZID 同样拒绝，不伪造无限历史/未来规则，不静默降为 floating。自定义 VTIMEZONE 保留不等于可执行其规划规则 |
| 尚未支持 | organizer、非空 attendees / alarms / attachments / extensions 整个事件显式拒绝；不静默丢弃。不能编码的 TZID 参数、控制字符、无效事件或元数据也拒绝 |

输出 UTF-8、CRLF；按 UTF-8 **字节数**折叠至每物理行最多 75 octets（含续行空格），不切开字符。TEXT 转义反斜线、逗号、分号与 LF；拒绝 CR 及不允许的控制字符。非 TEXT 字段不套用 TEXT 转义，内容行统一拒绝换行注入。输出按输入事件顺序，VTIMEZONE 按 TZID 排序；同快照输出确定。

## 验证证据

验收测试位于 `crates/calendar-adapters/tests/ics_rendering.rs`，golden 在其 `fixtures/render/`；局部 `.gitattributes` 保留协议 CRLF，不更改根目录规则。

| AC | 证据 |
| --- | --- |
| AC1 | 活动/删除黄金文件；四类时间与排他结束黄金文件；内部 ID 稳定、不同 ID 隔离、重复渲染确定、内部修订映射断言 |
| AC2 | 重复集合 / PERIOD / 单次取消 golden 及归一化 round-trip；缺时区、冲突定义、组件注入、override / ranged exception 拒绝 |
| AC3 | 多字节 Unicode 长行、标点/反斜线/换行转义、75 octets 断言；经生产 `normalize_ics` 还原语义，再渲染一致；只规范化导出 UID 与 SEQUENCE 这两项受控映射后比较事件正文 |

归一化基线 6/6 通过。首轮专项测试 4/6 通过、2/6 失败：有效源 VTIMEZONE 被拒绝。原因是验证探针使用 UTC DTSTART，归一化器按实际引用筛去未用时区；改为绑定待验证 TZID 的探针后 6/6 通过，并校对原组件与规范组件展开后的行，阻止额外组件注入。追加同名自定义时区跨事件误绑定回归，先观察 1/1 失败，再以每个 document 独立校验绑定修复；专项测试最终 7/7 通过。这是实际需求失败与修复，不把工具安装或编译错误当作 Red。

使用既有锁定的 `ical = 0.11.0` 与 `sha2 = 0.10`，无新增/升级依赖。适用的完整检查与远端审核结果记在 PR，未运行 fuzz、形式证明或真实客户端互操作测试；黄金与生产归一化器 round-trip 不声称全 RFC 或所有客户端兼容。

参考：通过 Exa 查阅 [RFC 5545](https://www.rfc-editor.org/rfc/rfc5545) §3.1（CRLF、75 octets 折叠）、§3.3.11（TEXT）、§3.6.5 / §3.2.19（VTIMEZONE / TZID）、§3.8.4.4 / §3.8.7.2 / §3.8.7.4（UID / DTSTAMP / SEQUENCE）、§3.8.4.2（RECURRENCE-ID）。使用仓库 #18 的解析选型和 #25 的受限生产归一化器作为语义回读边界。
