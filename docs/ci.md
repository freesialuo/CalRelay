# CI 触发与验证范围

CI 保留单一 required `quality-gate`。它验证当前 PR policy、快速契约检查及本次相关的 Rust/前端结果；失败、取消、未知路由或没有证据的 skip 不放行。没有修改 ruleset、必需审批或账号设置。

| 事件/改动 | 快速检查和当前 PR policy | Rust / 前端 | Docker |
| --- | --- | --- | --- |
| PR opened / synchronize / reopened | 合并为 checks job | 按完整 diff 保守路由 | 只对构建/验收输入运行 |
| PR edited（正文/标题等） | 始终重新验证，最终 gate 再读当前 metadata | 只有最新同 workflow/head/repo、同实际 merge SHA 的相关检查成功才复用；否则重新运行 | 不触发 |
| Ready | 不重复触发 CI | 不重复触发 | 不重复触发 |
| docs / Python 工具或测试 | 始终执行 | 通常跳过；CI 路由自身变更全量验证 | 非镜像输入跳过 |
| Rust / shared schema /生产前端或未知文件 | 始终执行 | 两者运行；仅前端测试改动运行前端 | 实际构建输入运行 |
| main push / 手动 CI | 始终执行 | 两者完整运行 | main 无自动发布 |
| 可信版本发布请求 / Release /手动镜像验证 | 原有精确来源/CI检查 | 候选 SHA 必须有确切 main push 成功证据 | 始终 build/smoke；发布还复核不可变制品与 registry guards |

路径不是整个 workflow 的顶层过滤器：即使 docs-only，聚合门禁仍会出现。改动集合由固定 base/head 的 git diff 读取，删除/重命名保留两端路径，浅克隆对象缺失或无法识别的输入回退全量；不能把“读不到 diff”当“无变化”。文档快速路径仅限指定根文档、docs Markdown 与 Issue/PR 模板；crate/src 中的 Markdown 等潜在构建输入不会被当成普通文档跳过。

metadata 复用不新增 workflow 或证明制品。`checks (实际 merge SHA)` 名称绑定本次 checkout 的合并版本，防止仅凭相同 head 借用旧 base 的结果；同时核对 GitHub API 中的 workflow、head、head repository、最新 run/attempt 及所需 job。最新失败/取消/运行中记录不能借用更旧绿灯。最终 gate 再验证该证据并重新读取当前 PR 正文/标题，不能用 metadata-only 绿灯覆盖失败代码。metadata 与 code 使用不同 concurrency group，改正文不会取消正在跑的代码检查；同类旧运行仍可取消。

Rust 两种 feature 构建分别使用固定 SHA 的 rust-cache，保存仅允许可信 main push；缓存命中仍执行格式/Clippy/tests 或真实服务构建，缓存不代表测试已通过。npm 保留 setup-node 的 package-lock 下载缓存及每次 `npm ci --ignore-scripts`，不缓存 node_modules 或浏览器状态。Fork PR 不获得写权限，不保存可信 Cargo cache；Docker 发布仍不使用 PR 构建缓存。

镜像工作流内先做轻量路径决定，没有增加调度 job。生产源、Dockerfile/context、构建配置及 smoke 变化仍 build/run；普通文档、Python gate 测试、明确排除的 Rust/前端测试文件不会重编译镜像。未知输入保守构建。可信 publication 不走 PR skip；read-only inspection 与 publish 的权限/触发分离保持原样。

公开源仓库与包可见性独立：只接受 `freesialuo/CalRelay` 和既定 main；源仓库 public 不会把 GHCR 改成 public。包仍须通过原 private/仓库关联、version/latest 不可变、精确 SHA CI 及制品校验；本轮没有触发诊断或发布。

本地验证：`python3 -m unittest discover -s tests -v`、`python3 scripts/check_repository.py`、actionlint。路由回归包括未知 diff、旧 base/head/repo/workflow/attempt、失败/取消和最终门禁复核；曾故意改成“选更旧成功 run”，回归产生真实断言失败，恢复后通过。

依据：[GitHub PR 事件语义](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request)、[rust-cache 缓存与保存控制](https://github.com/Swatinem/rust-cache)。实际节省取决于改动分布、cache 命中及 runner，未将预计节省冒充已测性能。
