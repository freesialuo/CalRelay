# 实现来源

| 贡献者 | 文件 | 来源与工具 | 用途及影响 | 验证 |
| --- | --- | --- | --- | --- |
| Codex（AI 辅助，按维护者需求） | `crates/calendar-storage/`、workspace/lockfile、存储与路线文档 | 原创实现；Context7 查询 SQLx 官方文档及迁移宏公共源码，链接见 [references.md](references.md#sqlite-存储实现) | Issue #21：文件数据库连接、内嵌迁移、约束与测试；后续 Issue 拆分追溯 | V-Model 对应关系和实际 TDD 过程见 [storage-foundation.md](storage-foundation.md)；未使用真实凭据或生产数据库 |
| Codex（AI 辅助，按维护者需求） | `calendar-application`、storage 同步实现/测试、需求模板及方法文档 | 原创实现；Context7 查询 SQLx 事务官方文档并核对公开源码 | Issue #22：原子同步、CAS、墓碑、失败恢复；为待办补充 US/验收映射 | 实际 TDD 记录及验证对应见 [source-sync.md](source-sync.md) |
| Codex（AI 辅助，按维护者需求） | `calendar-domain/src/recurrence.rs`、`tests/recurrence_golden.rs`、领域能力与参考文档 | 原创修复；Context7 查询 Chrono checked 运算文档并核对 0.4.45 公共源码、RFC 5545，见 [references.md](references.md#重复展开-review-修复pr-42) | Issue #20 / PR #42：修复继承跨度、年度月份展开、超大时长及生成年份越界 panic；修正 PERIOD 例外跨度优先级、日历时区按需校验及改期后的最终跨度检查 | Red/Green 与 AC1/AC2 回归见 [domain-capabilities.md](domain-capabilities.md#pr-42-review-修复) |
