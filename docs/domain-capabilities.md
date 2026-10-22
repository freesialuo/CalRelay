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
| RRULE 展开 | 评估候选 `rrule = 0.14.0`（`fmeringdal/rust-rrule`）后，选定由 `calendar-domain::recurrence` 基于已锁定 `chrono 0.4.45` / `chrono-tz 0.10.4` 实现受限日历坐标展开，不引入 `rrule` 外部依赖。输入边界 `validate_event` 保持原有语法级最小拒绝，不扩大校验能力声明；展开入口 `expand_occurrences` 仅执行已验证档位，显式拒绝未验证规则与超预算展开。 |
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

## 重复展开评估与支持边界（Issue #20）

候选对比结论：

- 核对 `rrule 0.14.0`（`fmeringdal/rust-rrule`）官方文档与公共源码发现：其 `src/iter/counter_date.rs::fix_day` 在闰日 `YEARLY` 递增时将 `2020-02-29` 偏移为 `2021-03-01`；`src/iter/utils.rs::add_time_to_date` 在午夜发生 DST 跳变（如 `America/Havana`）时因 `00:00:00` 非单一时刻而静默丢弃实例；`RRuleSet` 仅接受 `DateTime<Tz>` 的 `RDATE`，不支持 `RDATE;VALUE=PERIOD` 独立时长，也不含 `RECURRENCE-ID` 单次取消/覆盖补丁与 `original_key` 保持；`RRuleSet::all(limit)` 通过布尔标记截断而非返回类型化超限错误。
- 因此采用 `calendar-domain` 内置受限展开（`expand_occurrences`），在本地日历坐标生成候选日期，再统一经 `EventV1::resolve_utc` 与显式日历时区解析 UTC 区间：
  - 已验证档位：`FREQ=DAILY|WEEKLY|MONTHLY|YEARLY`、正整数 `INTERVAL`、`COUNT`、与 `start` 同族的 `UNTIL`、`WEEKLY` 无序号 `BYDAY`、`MONTHLY`/`YEARLY` 正数 `BYMONTHDAY`、`YEARLY` `BYMONTH`（均须与 `DTSTART` 同步），以及 `EXDATE`、`RDATE`（含 `Period` 独立跨度）与 `range=single` 的 `Cancelled`/`Override` 例外。
  - 显式拒绝策略（不扩大 `validate_event` 的 `RRULE` 校验声明）：`SECONDLY`/`MINUTELY`/`HOURLY`、带序号 `BYDAY`、负数 `BYMONTHDAY`、`BYSETPOS`、`BYYEARDAY`、`BYWEEKNO`、未同步 `DTSTART` 等返回 `Err(ExpansionError::UnsupportedRrule)`；`range=this_and_future` 返回 `UnsupportedRange`；来源 `VTIMEZONE` 返回 `UnsupportedTimezone`；缺少日历时区的 `date`/`floating` 返回 `MissingCalendarTimezone`；超过 `max_occurrences` 或 `max_iterations` 显式返回预算错误，不静默截断。

| 验收条件 | 设计与边界 | 可执行验证（`tests/recurrence_golden.rs`） |
| --- | --- | --- |
| AC1：月末、闰日、`COUNT`/`UNTIL`、`EXDATE`、`RDATE PERIOD` 与例外 | 不存在日历日跳过且不扣减 `COUNT`；`RDATE PERIOD` 保留独立时长；例外按 `original_key` 取消或改期 | `ac1_month_end_and_leap_day_skip_invalid_dates_without_drift_or_count_loss`、`ac1_count_and_until_boundaries_with_exdate_and_rdate_period`、`ac1_exceptions_cancel_or_reschedule_by_original_recurrence_key` |
| AC2：无效重复实例、时区绑定与有限窗口/数量预算 | 跨 DST（含午夜跳变）保持当地时间；`floating`/`date` 须提供 `calendar_tzid`；窗口/次数/步数超限显式报错 | `ac2_dst_transitions_and_timezone_bindings_preserve_local_wall_clock`、`ac2_finite_window_and_quantity_or_iteration_budgets_fail_explicitly` |
| AC3：记录选型及未通过项显式拒绝，不扩大当前 `RRULE` 校验声明 | `validate_event` 边界不变，`expand_occurrences` 对未验证子句返回 `UnsupportedRrule` | `ac3_unverified_rrule_clauses_and_ranges_are_explicitly_rejected_without_widening_validator` |

TDD 记录：

- Red：先定义 `expand_occurrences` 入口与 `tests/recurrence_golden.rs` 6 项测试，占位实现仅返回单次母事件；运行 `cargo test -p calendar-domain --test recurrence_golden --locked`，0 通过、6 失败（未展开候选序列、未应用 `EXDATE`/`RDATE PERIOD`/例外、未拒绝 `FREQ=HOURLY` 与超预算请求）。
- Green：实现受限 `RRULE` 解析、候选日生成、`EXDATE`/`RDATE PERIOD`/例外合并及预算检查；修正测试夹具中全天闰日事件误带 `PT1H` 的夹具设置（按开发方法不计作需求红灯），6 项测试全部通过。
- Refactor：增加 `DTSTART` 与 `BYDAY`/`BYMONTHDAY`/`BYMONTH` 同步性检查，并接入仓库示例 [event-v1.json](../examples/event-v1.json) 的完整展开断言。

### PR #42 review 修复

- 继承母事件 `end` 的每个实例均平移跨度：全天事件保持日历天数，定时事件保持准确时长；显式例外 `end` 与 `RDATE PERIOD` 保留独立跨度，显式 null 仍清除继承。
- `YEARLY;BYMONTHDAY` 未指定 `BYMONTH` 时遍历全年月份；指定 `BYMONTH` 时只遍历所选月份，无两者时继承开始月份。
- `validate_event` 仍只校验时长语法与符号；展开时数值解析、运算、Chrono 时长构造或日期范围溢出均返回 `ExpansionError::InvalidDuration`，不 panic。日历日相加后的年份须保持 Temporal 的四位格式。
- AC1/AC2 回归：`inherited_explicit_ends_preserve_every_instance_span_and_overlap`、`explicit_occurrence_ends_remain_absolute_and_cleared_ends_use_defaults`、`inherited_timed_ends_keep_exact_duration_across_dst`、`yearly_month_days_expand_all_months_unless_restricted`、`unrepresentable_durations_return_errors_for_all_span_sources`。
- Red：新增首批 4 项测试后，10 项中 7 通过、3 失败，分别复现一天实例缩为零、年度规则漏月及超大时长 panic。Green/Refactor：区分继承与显式结束时间，按年扩展月份，使用 checked 运算并合并日历日相加逻辑；补充跨 DST 回归后 11 项全部通过。
- 第二轮 review（AC2）：在候选日期转为 `Temporal` 并解析 UTC 之前检查四位年份范围，越界返回 `ExpansionError::InvalidWindow`；即使查询窗口很窄，也不会先解析五位年份而 panic。选择显式错误，不返回不完整集合。
- 回归 `generated_years_outside_temporal_range_return_typed_errors` 覆盖 `utc`/`zoned`/`floating`/`date` 和四种已支持频率的大 `INTERVAL`；`recurrence_year_limit_keeps_last_supported_year` 验证 9999 年可展开、10000 年被拒绝。Red：13 项中 11 通过、2 项均在 UTC 解析处 panic；Green：加上年份检查后 13 项全部通过。

- 第三轮 review（AC1/AC2）：例外显式设置非 null `end`/`duration` 时清除原有相反跨度字段，避免 `RDATE PERIOD` 时长遮蔽显式结束；null 只清除对应字段，纯元数据补丁保留 PERIOD 跨度。仅在实际解析 `date`/`floating` 值时校验日历时区，UTC/zoned 值忽略无关配置。
- 回归 `exception_spans_override_period_spans_without_shadowing` 覆盖显式结束/时长、null 和元数据补丁；`calendar_timezone_is_validated_only_for_temporals_that_need_it` 覆盖四种时间类型、缺省/未知/来源自定义日历时区。Red：15 项中 13 通过、2 失败，分别返回 11:00 而非显式 12:00、UTC 事件因未知日历时区报错；Green：修复后 15 项全部通过。

- 第四轮 review（AC1/AC2）：合并 PERIOD 与例外后，显式绝对结束若早于有效开始，返回 `ExpansionError::InvalidDuration`，不输出倒置区间；有效的绝对结束保持原值，定时事件开始等于结束仍允许。回归 `rescheduled_period_spans_are_checked_after_combining_overrides` 覆盖 UTC/floating/zoned 的无效改期，以及有效改期和相等边界。Red：16 项中 15 通过、1 失败，复现 12:00 开始、11:00 结束；Green：加上最终先后检查后 16 项全部通过。

## 测试

`cargo test -p calendar-domain` 的黄金样例位于 `crates/calendar-domain/tests/validation.rs`、`crates/calendar-domain/tests/timezone_golden.rs` 与 `crates/calendar-domain/tests/recurrence_golden.rs`，包含仓库示例 [event-v1.json](../examples/event-v1.json) 的往返、校验与重复展开验证。无网络通知。
