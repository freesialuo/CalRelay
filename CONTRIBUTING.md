# 开发流程

所有功能、修复、文档和 CI 修改都从 Issue 开始，通过分支和 PR 进入 `main`。

```text
Issue（范围与验收）→ 分支 → 实现与本地检查 → 提交 → PR
→ CI → 检查最终差异 → 维护者明确批准合并 → Merge
```

## Issue 与分支

先检索现有 Issue，避免重复。需求写目标、验收标准和依赖；缺陷写复现、实际行为和预期行为。不把尚未合并的依赖当作主线已具备的能力。

需求使用用户故事和 AC 编号，将验收场景对应到 V-Model 验证层级；实现按 TDD 的 Red → Green → Refactor 记录实际结果。准入、完成及 Rust 质量约定见 [开发方法](docs/development-method.md)。

从最新 `origin/main` 创建分支：`<type>/<issue>-<slug>`，type 为 `feat`、`fix`、`docs`、`test`、`ci`、`chore` 或 `refactor`，例如 `feat/12-ics-ingestion`。分支号必须是本仓库对应的 Issue。

PR 标题采用 `type(scope): description`，scope 可省略；正文写改动结果、实际验证与必要限制。完整交付（默认，或显式 `Delivery: full`）使用 `Closes #12`、`Fixes #12` 或 `Resolves #12` 关闭分支对应的 Issue。工作未完成时开 Draft PR。

已有父任务分支可以交付一个已完成的叶子范围，无需为通过校验而提前关闭父项。此时正文使用可见的独立文本行，例如：

```text
Delivery: partial
Refs #36
Closes #86
Remaining AC: #36 AC1, AC2, AC3 — 生产 runtime 注册与最终 CLI 配置接通仍由后续任务验收
```

部分交付必须恰好声明一次分支 Issue 的 `Refs`，以本仓库 `#编号` 语法关闭一个不同的、当前开放且没有子项的真实 Issue，并用一行 `Remaining AC` 列出父项 AC 编号（逗号分隔）及具体剩余责任。编号须存在于当前父 Issue；分支父项保持开放，正文任何位置均不得含其 closing 关键字。目标叶子若已有原生 parent，必须与分支 Issue 一致；原生关系缺失时这些字段只声明逻辑追踪，不代表新建了原生关系，维护者仍须核对叶子范围和本 PR 实际成果，以及提交消息/手工关联的关闭效果。

校验从 GitHub 读取当前 PR、分支 Issue 与叶子 Issue，读取失败不放行。注释、代码块、行内代码、引用和缩进代码中的字段不计入可见声明。部分模式不豁免标题、分支、main 目标、真实 Issue 类型、测试、CI 或合并授权；未声明该模式仍按完整交付校验。关键字冒号及跨仓库引用语义参照 [GitHub 官方说明](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue)；部分模式不接受限定仓库或 URL closing，以免绕过父项检查。

Dependabot 自动依赖 PR 是人工 Issue/分支命名规则的窄例外：校验器从当前 GitHub PR API 响应确认作者为 `dependabot[bot]` 且类型为 `Bot`、head/base 仓库 ID 相同且非空、分支以 `dependabot/` 开头并有后缀。仅免除 Issue 分支格式与 closing reference；标题格式、main 目标分支、全部测试和 quality-gate 保持不变。普通用户、其他 bot、fork 或身份信息缺失仍走普通规则，不能凭分支名、标签或正文获得例外。

`.github/dependabot.yml` 为根 Cargo workspace、`requirements/ci.txt` 和 GitHub Actions 每周检查版本更新，每生态最多 3 个打开的版本更新 PR。minor/patch 按生态分组，major 保持独立；分组不保证行为兼容，仍需审核和测试。Action 继续固定完整 SHA。配置在合并到默认分支后生效，不启用自动合并、不修改安全更新或访问设置；安全更新的触发与限额独立于版本更新。

配置依据：[Dependabot 选项](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference)、[支持的生态与 SHA 固定 Action](https://docs.github.com/en/code-security/reference/supply-chain-security/supported-ecosystems-and-repositories)。

## 提交与验证

CI 按相关改动运行，metadata/Ready 避免重复编译；main 始终完整验证，安全 skip、缓存及 required quality-gate 的规则见 [CI 触发与范围](docs/ci.md)。

提交签名为可选项，不作为贡献或合并的前置条件。

- 不强推。更新分支优先使用 merge 提交；如需重写历史，必须事先单独授权。
- 运行 `git diff --check`，安装 `requirements/ci.txt`，运行 `python3 -m unittest discover -s tests -v` 和 `python3 scripts/check_repository.py`。
- CI 运行事件 JSON Schema、示例、契约拒绝案例、PR 约定与仓库结构检查，以及 Rust workspace 的 `cargo fmt --check`、Clippy（`-D warnings`）和 `cargo test`；前端检查见 [管理前端](docs/admin-web.md)，包含真实本地 HTTP 与浏览器测试。
- Rust workspace 首次实现的同一 PR 必须接入 `cargo fmt --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings` 和适当的 `cargo test`；前端首次实现同样接入类型、构建与有价值的测试。所有检查纳入固定 `quality-gate`。
- Kani 是独立的可选工作流：相关路径改动时自动运行，也可手动触发；不纳入 `quality-gate` 或必需状态检查。范围与运行方式见 [形式验证](docs/formal-verification.md)。
- 按 Issue 的风险与 TDD 需要选用 [测试、证明与质量工具](docs/verification-toolkit.md)，记录适用范围及实际证据；允许的扩展能力不默认成为强制 CI。
- 核心日历逻辑优先黄金样例/性质测试；数据库事务与故障恢复做集成测试，Bark 用模拟服务验证。不得用真实外部发送代替自动测试。

## Review 与合并

沿用仓库已有自动审核：已触发则读取结果并处理意见，未触发不等待。不主动发送 Codex 代码或安全审核请求，也不因新 head 主动请求复审；不额外要求新回执或固定时长轮询。测试、CI、仓库实际必需检查与审核、讨论解决及维护者合并授权保持不变。

main 要求通过 PR、全部讨论解决及 `quality-gate` 成功；实际保护规则以 GitHub 当前 API 为准。当前未启用 strict/up-to-date 要求，单纯落后 main 不构成门禁；有真实冲突或共享契约集成风险时才同步并重新验证。管理员无日常绕过例外，禁止强推与删除 main。

本项目由一位维护者开发，不设置 GitHub 必需审批人数，不要求 code owner 或最新推送审批，也不模拟自我 approve。维护者检查最终差异与测试结果后，可在对话中明确批准该 PR 合并。

CI 成功不等于授权合并。代理仍须等待维护者对该 PR 的明确合并指令；这项授权是协作约定，不由 GitHub required review 强制。批准后若最终差异变化，需重新取得授权。默认关闭 auto-merge，只允许 merge commit，以保留原始提交历史。

合并后删除开发分支，由 closing keyword 关闭 Issue。

## CI 与仓库配置维护

GitHub Actions 权限默认只读，不允许 workflow 自行 approve PR；第三方 Action 固定完整 SHA。工作流使用 `pull_request`，不用 `pull_request_target` 执行不可信 PR 代码，不注入生产凭据。

`.github/main-ruleset.json` 保存 main 保护目标配置，实际生效状态以 GitHub API 为准，不能把配置文件存在当作保护已经启用。当前私有仓库是否支持强制规则取决于 GitHub 套餐。修改规则、检查名称或合并策略需同步文档并重新评审，不得在普通功能 PR 中悄悄放宽门槛。

本仓库不设置 CODEOWNERS 或强制 reviewer。
