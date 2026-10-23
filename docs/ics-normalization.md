# 受限 ICS 文件归一化（#25）

`calendar-adapters` 提供无网络、无数据库的受限 ICS 归一化入口 `normalize_ics` 与完整快照构造入口 `normalize_ics_to_snapshot_batch`，将已验证能力档位内的 RFC 5545 日历输入转换为通过 `validate_event` 校验的 `EventV1` 集合或 `SyncBatch`。HTTP 上传路由与远端拉取分别留给后续服务与协调器 Issue。

## 资源限制与整批失败边界

`IcsLimits` 在解析与转换全过程强制四项上限（默认：`max_bytes = 1 MiB`、`max_properties = 10_000`、`max_events = 1_000`、`max_parse_steps = 25_000`；任一上限为 0 或 `max_events > MAX_BATCH_RECORDS` 直接返回 `IcsError::BudgetExceeded { kind: InvalidLimits, .. }`）：

1. 字节上限在切分行前检查；行展开、属性解析、组件扫描、`VTIMEZONE` 与 `VEVENT` 转换统一扣减 `ParseSteps` 与 `Properties`，母事件计数扣减 `Events`。
2. 任一预算超限、语法损坏、能力未支持或单个 `VEVENT` 违反领域校验（`validate_event`）均使整批返回 `Err(IcsError)`，不产出部分 `SyncBatch`，从而防止全量快照的“缺失即删除”误删已有事件。
3. `normalize_ics_to_snapshot_batch` 仅构造 `SyncMode::FullSnapshot { complete: true, .. }` 与 `SyncRecord::Upsert` 记录，并在返回前调用 `SyncBatch::validate()`。

## 支持档位与显式拒绝策略

| 类别 | 已支持 / 保留 | 显式拒绝（`Malformed` / `Unsupported` / `DomainValidation` / `ContractViolation`） |
| --- | --- | --- |
| `VCALENDAR` | 单一顶层 `VCALENDAR`、`VERSION:2.0`、`PRODID`、可选 `CALSCALE:GREGORIAN`、`METHOD:PUBLISH`、`X-*` | 缺失/重复 `VERSION` 或 `PRODID`、非 `2.0` 版本、`METHOD:REQUEST` 等 iTIP 方法、非 `GREGORIAN` 历法、多顶层日历或未知属性 |
| 时间与跨度 | `DATE`、`UTC`、`floating`、`TZID` 绑定本地时间；`DTEND` 或 `DURATION`；`RDATE`（含 `VALUE=PERIOD` 的显式结束或独立时长）、`EXDATE` | `UTC` 或 `DATE` 附带 `TZID`、同时指定 `DTEND` 与 `DURATION`、结束早于开始、跨度类型不匹配、非法日期或时长 |
| 重复与例外 | 单条 `RRULE`；同一文件内关联母事件的 `RECURRENCE-ID`（默认/`SINGLE`）单次覆盖（`Change::Override`）或 `STATUS:CANCELLED` 单次取消（`Change::Cancelled`）；母事件使用 `DTEND` 而例外省略 `DTEND`/`DURATION` 时按母事件有效跨度归一化为 `duration` 并清除继承 `end`，母事件 `DTEND` 与例外 `DURATION` 互换时同样将相反继承字段置为显式 `null` | 孤立 `RECURRENCE-ID`、非重复母事件附带例外、例外 `VEVENT` 自带 `RRULE`/`RDATE`/`EXDATE`、同一 `VEVENT` 多条 `RRULE`、`RANGE=THISANDFUTURE`（映射后由 `validate_event` 返回 `Code::UnsupportedRange`） |
| `VTIMEZONE` | 包含至少一个 `STANDARD`/`DAYLIGHT` 子组件及 `TZID`/`DTSTART`/`TZOFFSETFROM`/`TZOFFSETTO`；按事件实际引用的 `TZID` 保存原始块到 `EventV1.timezones` | 缺失 `TZID` 或观测子组件、同一 `TZID` 定义冲突、未知子组件；规划/UTC 解析遇到非空来源 `VTIMEZONE` 仍显式返回 `Code::UnsupportedTimezone` |
| 文本、参与者、附件与告警 | 单遍 RFC 5545 TEXT 解码（`\\`、`\n`/`\N`、`\,`、`\;`）用于 `UID`、`PRODID`、`SUMMARY`、`DESCRIPTION`、`LOCATION` 与 `CATEGORIES`；支持 `URL`、`STATUS`、`CLASS`、`TRANSP`、`SEQUENCE`、`LAST-MODIFIED`、`ORGANIZER`、`ATTENDEE`、`ATTACH`（URI）、`VALARM`（`DISPLAY`/`AUDIO`/`EMAIL`，相对或 UTC 绝对触发）、`X-*` 扩展 | 未知反斜杠转义、尾随反斜杠、内联二进制 `ATTACH`（`ENCODING`/`VALUE=BINARY`）、`ACTION:PROCEDURE`、未知标准属性或参数 |
| 非 `VEVENT` 组件 | `VTODO`、`VJOURNAL`、`VFREEBUSY` 不混入 `events`，原样保留在 `NormalizedIcs.diagnostics`（含组件类型、可选 `UID` 与原始文本） | 其它未知顶层或嵌套组件 |

对 `STATUS:CANCELLED` 的例外 `VEVENT`，在折叠为 `Change::Cancelled` 前仍对其自身的 `DTSTART`、`DTEND`、`DURATION` 与 `VALARM` 执行独立领域校验，避免取消例外夹带非法时间或跨度被静默忽略。

## AC 与可执行验证

运行：`cargo test -p calendar-adapters --test ics_normalization --locked`。

| AC | 可执行测试（`crates/calendar-adapters/tests/ics_normalization.rs`） | 验证内容 |
| --- | --- | --- |
| AC1 | `ac1_golden_time_text_and_supported_properties_normalize_to_event_v1`、`ac1_golden_recurrence_exceptions_and_cancellations_preserve_recurrence_id_and_spans` | 复用 `#18` 的 `time-and-text.ics` 与 `recurrence-exceptions.ics` 黄金夹具，并覆盖富属性、单遍 TEXT 解码（字面 `\n` 不误转为换行）、`EXDATE`/`RDATE PERIOD`、按原始 `RECURRENCE-ID` 定位的改期与取消、母事件 `DTEND` 与例外 `DURATION` 互换后的 `expand_occurrences` 展开、大小写无关组件标签及空 `SUMMARY:` |
| AC2 | `ac2_byte_property_event_and_parse_step_budgets_reject_whole_batch`、`ac2_whole_batch_failure_prevents_partial_snapshot_deletion` | 零上限拒绝、字节/属性/事件/步数四类预算超限整批拒绝；构造完整快照 `SyncBatch`，验证多事件输入中任一事件校验失败时整批不生成快照 |
| AC3 | `ac3_timezone_and_thisandfuture_fixture_maps_vtimezone_and_fails_with_unsupported_range`、`ac3_non_vevent_components_are_retained_in_diagnostics_while_unsupported_or_malformed_vevents_fail` | `#18` 的 `timezone-and-range.ics` 保留 `VTIMEZONE` 并因 `THISANDFUTURE` 返回 `Code::UnsupportedRange`；单独自定义 `VTIMEZONE` 可存储归一化而在 `resolve_utc` 返回 `Code::UnsupportedTimezone`；`VTODO`/`VJOURNAL` 记入 `diagnostics`；损坏语法与不支持属性/方法/二进制附件显式报错 |

## TDD 与验证记录

- Red：新增 `calendar-adapters` crate 与 `tests/ics_normalization.rs` 6 项验收测试，占位 `normalize_ics` 返回未实现错误；运行 `cargo test -p calendar-adapters --test ics_normalization`，0 通过、6 失败。
- Green：实现有界行展开、组件栈与参数扫描、`ical 0.11.0` 结构复核、属性转换与 `validate_event` / `SyncBatch` 校验；修正母事件 `DTEND` 被例外 `DURATION` 覆盖时的继承清除逻辑后，6 项测试全部通过。
- Refactor：补充大小写无关 `BEGIN`/`END`、空属性值在 `ical 0.11.0` 复核流中的兼容处理，以及 `STATUS:CANCELLED` 例外 `VEVENT` 的独立领域校验；对 `BudgetTracker::event` 执行受控源码 mutation（移除 `max_events` 超限分支），确认 `ac2_byte_property_event_and_parse_step_budgets_reject_whole_batch` 精确检出失败后恢复生产实现。
- PR #54 review 修复：
  - Red：在 `ac1_golden_recurrence_exceptions_and_cancellations_preserve_recurrence_id_and_spans` 中新增母事件含 `DTEND` 而改期例外省略 `DTEND`/`DURATION`（含 UTC 定时与全天 `VALUE=DATE`）以及 `UID` 含 RFC 5545 TEXT 转义序列的回归断言；运行 `cargo test -p calendar-adapters --test ics_normalization --offline --locked`，复现 `EndBeforeStart` 失败与未解码 `UID`。
  - Green/Refactor：在 `extract_vevent`、`build_retained_diagnostics` 与 `validate_vcalendar_properties` 中对 `UID`/`PRODID` 先执行单遍 `unescape_text` 再校验长度与分组；当母事件指定 `DTEND` 且改期例外省略 `DTEND`/`DURATION` 时，在同时间族下将母事件跨度归一化为例外补丁的 `duration` 并显式清除继承 `end`，6 项测试全部通过。

## 实现参考

通过 Exa 核对锁定的 `ical 0.11.0` 公共源码与 RFC 5545 规范：

- [ical 0.11.0 `src/line.rs`](https://raw.githubusercontent.com/Peltoche/ical-rs/v0.11.0/src/line.rs)、[`src/property.rs`](https://raw.githubusercontent.com/Peltoche/ical-rs/v0.11.0/src/property.rs)、[`src/parser/ical/component.rs`](https://raw.githubusercontent.com/Peltoche/ical-rs/v0.11.0/src/parser/ical/component.rs)：核对发现 `LineReader` 会裁剪物理行尾空白并跳过空行，`PropertyParser` 限制最多 10 个逗号参数值且在属性值长度 `<= 1`（即空属性值 `PROP:`）时报错，`Component::parse` 不校验 `END` 组件名与 `BEGIN` 是否配对。因此接入层先在有界预扫描中完成严格行展开、配对栈检查与属性提取，再调用 `ical::IcalParser` 复核日历结构。
- [RFC 5545](https://www.rfc-editor.org/rfc/rfc5545.html)：§3.1 内容行与折叠、§3.2 属性参数、§3.3.11 TEXT 转义、§3.6.1 `VEVENT`、§3.6.5 `VTIMEZONE`、§3.6.6 `VALARM`、§3.8.4.4 `RECURRENCE-ID`。
