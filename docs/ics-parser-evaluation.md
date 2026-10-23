# ICS 解析库评估（#18）

本次只评估解析后的数据保留，不实现生产接入器或扩大领域支持范围。候选仅作为 `calendar-domain` 的开发依赖：`ical = 0.11.0`（`ical` feature）、`icalendar = 0.17.6`（`parser` feature），均固定版本并纳入 `Cargo.lock`。

## 样例与验收

运行：`cargo test -p calendar-domain --test ics_parser_golden --locked`。固定输入位于 `crates/calendar-domain/tests/fixtures/ics/`，断言位于 `tests/ics_parser_golden.rs`；`tests/support/ics_evaluation.rs` 只将候选的属性和子组件投影到可比较结构，不转换为 `EventV1`。

| AC | 样例与判定 |
| --- | --- |
| AC1 | `time-and-text.ics`：date/UTC/floating/zoned 的字面值及 VALUE/TZID、Unicode、TEXT 转义、空格及 tab 折叠、重复属性与重复扩展参数 |
| AC2 | `recurrence-exceptions.ics`：EXDATE 列表与重复属性、两种 RDATE PERIOD 跨度、按原始 RECURRENCE-ID 定位改期和取消 |
| AC2 | `timezone-and-range.ics`：VTIMEZONE 的 STANDARD/DAYLIGHT、偏移及 RRULE，以及 RECURRENCE-ID 的 TZID/RANGE=THISANDFUTURE |
| AC3 | 两候选运行相同输入，以独立固定值判断，不以候选之间相等作为唯一 oracle；版本、参考和结论保留于本文 |

## 支持边界

选定 `ical 0.11.0` 作为后续接入器的低层解析候选：本矩阵中的时间字面值、参数、重复属性和时区子组件均保留，TEXT 值保持原始转义，后续需按属性类型正确解码。它按组件种类分组，不能依赖跨种类的原始顺序；解析成功也不代表字段语义有效。

`icalendar 0.17.6` 在本矩阵中同样保留时间、重复属性与子组件，TEXT 则在解析时解码。但其连续替换会将编码为两个反斜杠再加 `n` 的字面反斜杠加 `n` 误解为换行；`ac3_literal_backslash_n_exposes_icalendar_text_decoding_loss` 固定该反例，测试断言实际损失，不在 helper 中修补后宣称兼容。因此未选为首版输入解析库。

解析器保留 `RRULE`、`RDATE`、`VTIMEZONE` 或 `THISANDFUTURE` 不代表能够执行其语义。现有领域层对自定义 VTIMEZONE 和 THISANDFUTURE 的明确拒绝保持不变，见 [领域能力](domain-capabilities.md)。TEXT 解码、事件归一化、语义校验和资源限制属于后续 [#25](https://github.com/freesialuo/CalRelay/issues/25)，不能仅凭本样例矩阵接收任意 ICS。

## TDD 与协作记录

- Red：测试 agent 先写六项固定预期；可编译占位 helper 返回空日历。运行 `cargo test -p calendar-domain --test ics_parser_golden`，0 通过、6 失败，原因是缺少 VEVENT/VTIMEZONE 等组件。不是编译或夹具错误，也不是候选库自身全部失败。
- Green：实现 agent 收到 Red 后实现两候选的属性树投影；补充转义反例后，`cargo test -p calendar-domain --test ics_parser_golden --locked` 为 7 通过、0 失败。测试 agent 独立复核结果相同。
- Refactor：格式化测试与 helper，样例按仓库约定保存 LF，测试统一转换为 CRLF 后交给两候选；保持候选原始解析值，不用统一解码掩盖差异。
- 2026-10-22 UTC 时间采样：测试 agent 首次采样 17:18:16，独立 Green 完成 17:21:37（3 分 21 秒，不含此前阅读规范）；实现 agent 在 Red 后的实现及目标检查为 17:20:51–17:21:20（29 秒）。两者并行准备样例、API 和候选研究，Red→Green 交接仍顺序进行。没有单 agent 对照，不能据此量化提速。

本地检查通过：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、`cargo test --workspace --all-features --locked`（81 项）、`python -m unittest discover -s tests -v`（11 项）、`python scripts/check_repository.py` 和 `git diff --check`。Python 使用临时虚拟环境安装 `requirements/ci.txt`；尚未运行远端 CI。

## 实现参考

已尝试通过 Context7 检索 Rust `ical` / `icalendar`；索引未返回相关 Rust API，未将不相关结果作为依据。随后核对所下载固定版本的官方文档及公共源码：

- [ical 0.11.0](https://docs.rs/ical/0.11.0/ical/)、[Property 文档](https://docs.rs/ical/0.11.0/ical/property/index.html)、[源码](https://github.com/Peltoche/ical-rs/tree/v0.11.0)：`IcalParser`、属性参数、行展开及结构化组件；属性解析不负责字段语义有效性。
- [icalendar 0.17.6 parser](https://docs.rs/icalendar/0.17.6/icalendar/parser/)、[组件源码](https://docs.rs/icalendar/0.17.6/src/icalendar/parser/components.rs.html)、[ParseString 源码](https://docs.rs/icalendar/0.17.6/src/icalendar/parser/parsed_string.rs.html)：使用 `unfold` 与低层 `read_components`，避免转换为高层构建器后再评价保留能力。
- [RFC 5545](https://www.rfc-editor.org/rfc/rfc5545.html)：§3.1 折叠与内容行、§3.3.11 TEXT、§3.6.5 时区、§3.8.4.4 例外标识、§3.8.5 重复属性作为固定样例依据。
