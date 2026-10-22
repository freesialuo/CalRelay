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
| TZID 须为 IANA，或由来源 `timezones` 提供非空定义；否则拒绝，不默认 UTC | `UnknownTimezone` |
| 来源 `VTIMEZONE` 绑定的 TZID（含与 IANA 同名冲突）尚未解释，规划/UTC 解析显式拒绝 | `UnsupportedTimezone` |
| EXDATE/RDATE/RECURRENCE-ID 与母事件同类型、同 TZID | `RecurrenceTypeMismatch` |
| 例外以原始 `recurrence_id` 的规范 key 定位，重复定位拒绝 | `DuplicateException` |
| `range=this_and_future` 显式不支持，不当作单次覆盖 | `UnsupportedRange` |
| RRULE 须含合法 `FREQ`，`COUNT` 与 `UNTIL` 互斥 | `UnsupportedRrule` |
| 告警 offset / repeat_interval 语法，绝对触发须为 UTC | `InvalidDuration` 等 |

覆盖校验以 `patch.start` 或该例外的 `recurrence_id` 为开始；`end` / `duration` 缺省继承母事件字段，显式 null 清除，继承后仍须互斥。继承的 `end` 保留原值，不在校验器中展开或平移；如其早于例外开始，须显式覆盖或清除。

zoned 值解析遵循 RFC 5545：歧义时间取第一次出现，不存在的时间通过 `chrono_tz::GapInfo` 读取转换前生效偏移（`Temporal::to_utc`、`Temporal::to_utc_with_timezones`、`EventV1::resolve_utc`）。floating 与全天值在校验字面值合法后返回 `Ok(None)`，由规划层配置解释时区，不回退宿主机时区。来源 `VTIMEZONE` 绑定的 TZID 暂不能转换为绝对时刻，即使与 IANA 名称相同也优先保留来源定义：存储校验（`validate_event`）在跨度任一端绑定非空来源定义时跳过绝对先后比较，而规划/UTC 解析（`resolve_utc` / `to_utc_with_timezones`）显式返回 `Err(Code::UnsupportedTimezone)`，不回退同名 IANA 或宿主机时区；若来源声明了同名条目但 `vtimezone` 为空白，则校验与解析均按 `Code::UnknownTimezone` 拒绝。

## 库评估记录

| 能力 | 当前结论 |
| --- | --- |
| 时区数据库 | 锁定 `chrono = "=0.4.45"`、`chrono-tz = "=0.10.4"`（编译期嵌入 IANA 数据，公开 `TZDB_VERSION = "2025b"`）。覆盖南北半球与午夜 DST 间隙/重叠、30/45 分钟偏移及 30 分钟 DST 跳变、跨日/跨月/闰日/整日国际日期变更线跳变与同名 `VTIMEZONE` 冲突；数据库更新需重规划未来实例（见 [event-model.md](event-model.md)）。 |
| ICS 解析 | 未选型，不在本 Issue 范围；由 [#18](https://github.com/freesialuo/CalRelay/issues/18) 依据 [event-model.md](event-model.md) 的黄金样例清单评估。 |
| RRULE 展开 | 未选型；本阶段只做语法层面的最小拒绝。在 [#20](https://github.com/freesialuo/CalRelay/issues/20) 展开能力评估通过前，不得声称支持任何超出 `FREQ` 校验的规则语义。 |
| 自定义 VTIMEZONE | 仅保存与非空存在性校验，不解释其规则；规划/实例解析遇到来源 `VTIMEZONE`（含同名 IANA 冲突）显式返回 `UnsupportedTimezone`。 |

## 时区兼容性验证（Issue #19）

| 验收条件 | 设计与边界 | 可执行验证（`tests/timezone_golden.rs`） |
| --- | --- | --- |
| AC1：DST 间隙/重叠、非整小时转换、跨日变更、同名冲突 | `GapInfo` 读取跳变前偏移；`Ambiguous` 取首次出现；来源 `timezones` 优先于同名 IANA | `ac1_dst_gaps_and_overlaps_across_hemispheres_and_midnight_transitions`、`ac1_non_whole_hour_offsets_and_half_hour_dst_transitions`、`ac1_cross_day_boundaries_and_dateline_full_day_skips`、`ac1_and_ac2_same_name_source_vtimezone_conflicts_never_fall_back_to_iana` |
| AC2：自定义 `VTIMEZONE` 未解释时明确拒绝规划，不回退宿主机或同名 IANA | `to_utc_with_timezones` / `EventV1::resolve_utc` 返回 `Err(Code::UnsupportedTimezone)`；未知时区与空白同名定义返回 `Err(Code::UnknownTimezone)`；`date`/`floating` 返回 `Ok(None)` 并校验格式 | `ac1_and_ac2_same_name_source_vtimezone_conflicts_never_fall_back_to_iana`、`ac2_uninterpreted_custom_vtimezone_and_unknown_zones_are_rejected_without_host_fallback` |
| AC3：固定版本并记录案例、限制与失败处理 | `Cargo.toml` 锁定 `chrono = "=0.4.45"`、`chrono-tz = "=0.10.4"`；导出 `TZDB_VERSION`（`"2025b"`） | `ac3_pinned_tzdb_version_and_deterministic_golden_matrix` |

TDD 记录：

- Red：新增 `tests/timezone_golden.rs` 6 项黄金样例测试并运行 `cargo test -p calendar-domain --test timezone_golden --locked`，4 通过、2 失败：同名来源 `VTIMEZONE`（`America/Vancouver`）被解析回退为 IANA `2026-11-02T17:00:00Z`，自定义 `VTIMEZONE` 与未知时区在 `to_utc` 中返回 `Ok(None)` 而非显式错误。
- Green：在 `Temporal::to_utc_with_timezones` 与 `validate_event` 中优先判定来源 `timezones`（非空返回 `UnsupportedTimezone`，空白定义或未知 TZID 返回 `UnknownTimezone`），并使用 `chrono_tz::GapInfo` 解析不存在的本地时间，6 项测试全部通过。
- Refactor：补充跨 45 分钟偏移时区（`Asia/Kathmandu` 与 `Australia/Eucla`）的跨度先后校验，统一 `validate_event` 的跨度比较入口并运行全工作区回归检查。

## 测试

`cargo test -p calendar-domain` 的黄金样例位于 `crates/calendar-domain/tests/validation.rs` 与 `crates/calendar-domain/tests/timezone_golden.rs`，包含仓库示例 [event-v1.json](../examples/event-v1.json) 的往返与验证。无网络通知。
