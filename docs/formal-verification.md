# 重复展开的有界形式验证

Issue [#45](https://github.com/freesialuo/CalRelay/issues/45) 是 PR [#42](https://github.com/freesialuo/CalRelay/pull/42) 的验证补充；不扩展 RRULE 支持档位或改变展开行为。

## 执行

```sh
cargo install --locked kani-verifier --version "=$(cat scripts/kani-version)"
cargo kani setup
python3 scripts/check_kani.py
cargo test -p calendar-domain --test recurrence_bounds --locked
```

固定 Kani 0.68.0；其 setup 安装对应 Rust nightly 和 CBMC。CI 的 `kani` 作业执行证明及故障注入，结果为固定 `quality-gate` 的必需输入。现有 Rust 全量回归仍由 `rust` 作业执行。

## 验证范围与 AC 映射

| AC | 可执行验证 | 输入及声明 |
| --- | --- | --- |
| AC1 | `budget_step` | 当前目标平台全部 `usize` 值，前置条件 `remaining <= max_iterations`；有预算则恰好扣 1，否则返回携带原上限的错误、保持 0；上限不变。unwind=2，用于错误类型析构中的不可达集合循环 |
| AC1 | `budget_exhaustion` | 初始预算 0..=4，完全消耗后再消费两次均拒绝且保持 0；unwind=6 |
| AC1 | `recurrence_bounds` 集成测试 | DAILY COUNT=1..=4，重复 RDATE 数 0..=4，EXDATE 开/关，预算 1..=9：360 组；验证先计费再去重/排除、错误上限、成功集合与原始身份。属于枚举测试，不是形式证明 |
| AC2 | `window_membership` | 四个独立 i8 秒值（Unix epoch 前后 -128..=127 秒），前置条件 start<=end、window_start<window_end；调用实际 Chrono UTC 构造与生产窗口函数，与不相交的补集规格对照；包括零时长和相邻边界，unwind=2 |
| AC3 | `scripts/check_kani.py` | 临时副本将扣 1 改成扣 0，以及区间右边界 `<` 改成 `<=`；分别要求预算/窗口的命名断言产生反例 |
| AC4 | CI / `quality-gate` | 先要求全部三个 harness 成功，再分别验证两个 mutant；版本不符、编译/工具失败、unwind 失败或缺少目标断言反例均不能算检出缺陷 |

所有 harness 调用生产函数，不复制待验证实现；未使用 stub、跳过内存/溢出/unwind 检查或实验性循环契约。前置条件保留成功与拒绝路径。故障注入仅改临时副本，使用 `find … -delete` 清理。

可信边界包括 Kani/CBMC、编译器及被调用依赖的实现；未证明完整展开器、任意集合长度、所有日期/精度、DST、Chrono 或时区数据库本身。窗口证明只覆盖上述整秒范围；时区及完整领域语义继续依赖 [黄金样例与能力边界](domain-capabilities.md)。

## 实际验证记录

2026-10-22，Apple Silicon（64 位 `usize`），Kani 0.68.0 / CBMC 6.11.0：

- Red：预算 mutant 触发 `budget must decrease by one` 与 `exhausted budget must reject`；窗口 mutant 触发 `half-open window membership must match specification`，均为实际断言反例。
- 门禁测试 Red：不同断言名含目标名称子串时被错误接受；改为失败断言描述集合精确匹配后 Green。编译失败、unwind 失败、成功退出、缺失断言、额外失败及零 harness 不能冒充验证成功。
- Green：三个原始 harness 全部通过；两个源码 mutant 均被检出；360 组实际展开组合通过。
- 回归：Rust workspace 74 项测试、fmt、Clippy `-D warnings`、11 项 Python 测试、仓库检查及 `git diff --check` 通过。

首次工具试跑因错误枚举析构中的集合循环需要显式 unwind；补充上限后保留默认检查并通过。该工具配置问题不计为需求 Red。CI 的 Linux 运行结果以对应 PR 为准。
