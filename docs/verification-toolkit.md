# 按风险选用的测试、证明与质量工具

本项目允许在相关 Issue 中按需要引入下列能力。工具服务于 AC 和具体风险，不要求每个 Issue 使用全部工具，也不因列入本文而视为已安装、已接入或已通过。同类工具可按相同原则在相关 Issue 中评估和引入。

## 在 TDD 中选用

1. 定义要防止的失败与可观察结果，将 AC 对应到 [V-Model](development-method.md#v-model) 的适当验证层级；先选能覆盖风险的最小方案。
2. 在 Issue 的验证计划中写明所选工具、验证对象、输入/模型范围、判定依据、执行预算与运行方式。普通边界问题可用单元/集成测试解决；状态空间、并发或内存风险再选对应工具。无需为未选工具逐项写排除理由。
3. 对行为实现先保留有意义的 Red（失败测试、缩减输入、反例轨迹、未满足的证明义务等），修复后 Green，再 Refactor 并回归。编译失败、工具不可用、超时或错误夹具不算需求 Red；纯文档和工具选型不编造 Red。
4. 将发现的缺陷转换为可重放回归；工具版本、命令、目标平台/features、种子/corpus、假设与界限随结果记录。只保存脱敏的小型夹具和必要证据，不提交真实运行数据库或敏感输入。
5. 引入新验证方案时，用已知错误或受控 mutation 检查它能检出目标缺陷；不能把构建失败当作有效检出。PR 写明实际运行、跳过/不支持的部分与剩余限制。

测试、性质、模型和证明应对应生产实现或明确标注的规格；若另建模型，说明它与代码的映射，并用集成测试或轨迹复核关键行为。证明模型正确不能直接声称生产系统正确。

## 允许使用的能力

| 能力 / 工具（官方参考） | 何时使用、项目中的候选对象 | 应保留的证据与边界 |
| --- | --- | --- |
| 单元、集成与文档测试：[cargo test](https://doc.rust-lang.org/cargo/commands/cargo-test.html) | 领域规则、错误分支、黄金样例、真实 SQLite 事务与模拟适配器；作为 TDD 常规起点 | 正常/失败/边界结果；单元测试不能代替真实事务、网络模拟及恢复测试 |
| 属性测试：[proptest](https://github.com/proptest-rs/proptest) | 时间/重复规则组合、归一化幂等、编解码 round-trip、来源隔离不变量 | 输入策略、种子、shrinking 后的最小反例及 regression 文件；抽样通过不是穷举证明，关键极值仍写固定用例 |
| Fuzz：[cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) | ICS/JSON/重复规则等不可信输入的解析与转换 | fuzz target、脱敏 corpus、时间/内存预算和缩减 crash；除 panic 外按需求检查语义/资源限制，未发现 crash 不等于完整正确 |
| 有界模型检查：[Kani](https://model-checking.github.io/kani/tutorial-loop-unwinding.html) | 可隔离的预算、算术、窗口及小规模控制流 | 生产函数 harness、前置条件、输入/unwind 范围与反例；保留默认安全检查，不把有限输入证明推广到整个算法；现有范围见 [Kani 验证](formal-verification.md) |
| 演绎证明：[Verus](https://verus-lang.github.io/verus/guide/) | 高价值且规格稳定的纯算法、循环不变量及前/后置条件 | 规格、证明义务、终止性范围和可信假设；`external_body`/外部规格属于未证明边界，未验证依赖不能被算入证明 |
| 状态机模型：[TLA+ / TLC](https://lamport.azurewebsites.net/tla/tools.html) | 来源 CAS、规划水位、租约/重试、取消与崩溃恢复协议 | 状态/动作、有限模型配置、安全性/活性及公平性假设、反例轨迹；TLC 检查所选规格实例，不直接证明 Rust/SQL 实现 |
| 并发模型检查：[Loom](https://docs.rs/loom/latest/loom/) | 自有共享内存同步、原子操作、锁或线程交错；仅在实际引入此类代码时使用 | 用 Loom 原语建模、线程/调度限制、可复现交错；不能直接穷举真实 Tokio/SQLite/系统调用，数据库竞争仍做集成测试 |
| Mutation testing：[cargo-mutants](https://mutants.rs/) | 检查领域规则、错误分支及新测试方案是否能阻止有意义的错误 | baseline 先通过，报告 caught/missed/unviable/timeout；分析等价 mutation，不把构建失败或超时当作测试杀死 mutant，不盲目追求分数 |
| 性能回归：[Criterion](https://bheisler.github.io/criterion.rs/book/) / benchmark CI | 重复展开、批量同步、查询等存在可测性能需求的路径 | 代表性负载、固定环境/版本、baseline、噪声与阈值；吞吐/尾延迟需求另做对应系统测量，共享 runner 的单次波动不能直接判回归 |
| Dependency policy：[cargo-deny](https://embarkstudios.github.io/cargo-deny/) | 新增/升级依赖或依赖策略维护 | 明确 advisories、licenses、bans、sources 策略及目标/features；例外记录原因、范围与复查期限，通过不代表没有未知漏洞 |
| API compatibility：[cargo-semver-checks](https://github.com/obi1kenobi/cargo-semver-checks) | 已约定稳定性的 Rust 公共 API 变更或发版 | 指定 Git/发布版本 baseline、目标/features 与版本策略；当前私有未发布 workspace 按实际接口承诺选用，不代替 HTTP/JSON 契约或行为兼容测试 |
| UB 检测：[Miri](https://github.com/rust-lang/miri) | unsafe、别名/生命周期、原始指针或疑似未定义行为的 Rust 路径 | 固定 nightly、实际执行路径和可重现输入；受支持的测试子集通过不代表所有输入无 UB，FFI/系统调用不支持处单独说明 |
| Supply-chain：SBOM + [artifact attestation](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations) | 二进制/容器发布与依赖追溯；SBOM 可选 [Syft](https://github.com/anchore/syft) 等合适生成器 | SBOM 覆盖实际制品及容器 OS 包，采用 SPDX/CycloneDX 等格式；证明绑定制品 digest、构建来源并验证身份/策略，记录生成与验证命令；来源证明不等于安全或正确性证明 |
| Migration testing：old DB → latest schema | 新增 SQLite schema 迁移、兼容升级及恢复路径 | 用已支持历史 schema 生成合成旧库与代表性数据，经真实迁移入口升级；验证数据/外键/索引/版本、重开/重入及失败保留；新空库初始化或同版重开不算历史升级测试 |

## 运行与接入约定

常规 `cargo test`、fmt、Clippy 及既有仓库检查继续遵守 [贡献指南](../CONTRIBUTING.md)。其余扩展工具优先局部、按路径、手动或专项工作流运行；长时间 fuzz、mutation、模型探索和 benchmark 可在 Issue 明确预算后定时运行。低成本属性回归可以随所属 crate 的 `cargo test` 执行，不另设统一工具门禁。

这些能力不默认加入 `quality-gate` 或 GitHub 必需检查。Issue 可以将相关验证作为自身 AC；若要变更仓库合并门禁，单独说明范围、运行成本及失败策略，按既有维护流程评审。Kani 保持独立可选工作流，跳过/失败/等待不影响常规门禁。

工具接入时锁定版本，核对平台/nightly/FFI 支持并保留官方资料。发布证明所需的凭据与权限只配置在相应发布工作流；普通 PR CI 不因本目录而获得发布权限。GitHub attestation 是否可用须按仓库可见性、套餐和权限在发布实现时复核。

## 当前状态与后续选用

当前已接入 `cargo test`、领域黄金样例、SQLite 事务/故障测试与独立可选 Kani；现有 Kani 受控源码 mutation 不等于已接入 `cargo-mutants`。其余表列工具均为允许按需引入的能力，未宣称已安装或运行。

存储目前只有 `0001` 初始迁移，[存储基础测试](storage-foundation.md) 覆盖初始化、重开和异常历史；未来新增迁移时补充 old DB → latest schema 测试，不能将现有测试表述为已验证跨版本升级。发布 SBOM/attestation 随 [#38](https://github.com/freesialuo/CalRelay/issues/38) 等制品任务按需交付。

候选映射：领域/ICS Issue 可选择属性测试与 fuzz；[#23](https://github.com/freesialuo/CalRelay/issues/23)/[#24](https://github.com/freesialuo/CalRelay/issues/24) 可选择状态机模型及局部证明；并发原语、unsafe、公共 API、依赖、迁移或性能目标出现时选对应工具。上述为设计选项，不改变已有 Issue 的验收范围；实施时更新该 Issue 的验证计划。
