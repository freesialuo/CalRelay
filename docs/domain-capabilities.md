# 领域能力与支持边界

实现位置：`crates/calendar-domain`（无网络、无数据库、无 I/O）。契约来源：[event-v1.schema.json](../schemas/event-v1.schema.json)，语义见 [event-model.md](event-model.md)。

## 已实现的验证

| 规则 | 错误码 |
| --- | --- |
| `end` 与 `duration` 互斥（事件、覆盖补丁、RDATE PERIOD） | `EndAndDurationExclusive` |
| 日期/时间值合法（`2027-02-29`、`25:00:00` 拒绝，闰日在闰年通过） | `InvalidTemporal` |
| 结束与开始同族：全天↔全天、floating↔floating、UTC/zoned 互通 | `EndTypeMismatch` |
| 结束不早于开始（全天结束日排他，须晚于开始日；UTC/zoned 按绝对时刻比较） | `EndBeforeStart` |
| duration 语法；跨度不得为负；全天 duration 不得含时间部分 | `InvalidDuration` / `NegativeDuration` / `DateDurationHasTime` |
| TZID 须为 IANA，或由来源 `timezones` 提供定义；否则拒绝，不默认 UTC | `UnknownTimezone` |
| EXDATE/RDATE/RECURRENCE-ID 与母事件同类型、同 TZID | `RecurrenceTypeMismatch` |
| 例外以原始 `recurrence_id` 的规范 key 定位，重复定位拒绝 | `DuplicateException` |
| `range=this_and_future` 显式不支持，不当作单次覆盖 | `UnsupportedRange` |
| RRULE 须含合法 `FREQ`，`COUNT` 与 `UNTIL` 互斥 | `UnsupportedRrule` |
| 告警 offset / repeat_interval 语法，绝对触发须为 UTC | `InvalidDuration` 等 |

覆盖校验以 `patch.start` 或该例外的 `recurrence_id` 为开始；`end` / `duration` 缺省继承母事件字段，显式 null 清除，继承后仍须互斥。继承的 `end` 保留原值，不在校验器中展开或平移；如其早于例外开始，须显式覆盖或清除。

zoned 值解析遵循 RFC 5545：歧义时间取第一次出现，不存在的时间使用转换前的偏移（`Temporal::to_utc`）。floating 与全天值不产生绝对时刻，由规划层配置解释时区。来源 VTIMEZONE 绑定的 TZID 暂不能转换为绝对时刻，即使与 IANA 名称相同也优先保留来源定义：跨度任一端绑定来源定义时跳过绝对先后比较，规划前须另行解释。`Temporal::to_utc` 不接收来源上下文，仅用于明确采用 IANA 语义的调用。

## 库评估记录

| 能力 | 当前结论 |
| --- | --- |
| 时区数据库 | 采用 `chrono-tz`（编译期嵌入 IANA 数据）。由 DST 间隙/重叠样例验证；数据库更新需重规划未来实例（见 event-model）。 |
| ICS 解析 | 未选型，不在本 Issue 范围；由接入 Issue（#3）依据 [event-model.md](event-model.md) 的黄金样例清单评估。 |
| RRULE 展开 | 未选型；本阶段只做语法层面的最小拒绝。在展开能力评估通过前，不得声称支持任何超出 `FREQ` 校验的规则语义。 |
| 自定义 VTIMEZONE | 仅保存与存在性校验，不解释其规则；不支持项不会被悄悄当作 UTC。 |

## 测试

`cargo test -p calendar-domain` 的黄金样例位于 `crates/calendar-domain/tests/validation.rs`，包含仓库示例 [event-v1.json](../examples/event-v1.json) 的往返与验证。无网络通知。
