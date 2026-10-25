# 镜像构建与版本发布

此流程属于 #108，支撑 #38 / #78 的镜像交付范围。它不代表生产闭环已验收，也不部署服务器。镜像为 `ghcr.io/freesialuo/calrelay`，首版只验证 `linux/amd64`。Dockerfile、Compose 与真实镜像验收由 #105 管理。

## 触发与权限

- 面向 main 的 PR：只构建和运行真实镜像验收，不登录 GHCR、不推送，不取得 packages 写权限。
- Release `published`：只接受 `vMAJOR.MINOR.PATCH[-prerelease]`，Release 的 prerelease 标志必须与版本一致。
- 手动 `workflow_dispatch`：必须从 main 运行，提供已存在的版本 tag；`publish` 默认为 false，仅验证。显式 true 才发布。

没有 main push 自动发布，没有 `pull_request_target` 或 `workflow_run` 制品晋级。发布 job 仅允许本仓库的上述可信事件。Action 固定完整 commit SHA；构建没有 secret/buildargs，不共享不可信 PR 构建缓存。只有独立发布 job 有 `packages: write`，使用临时 `GITHUB_TOKEN`；不新增 PAT，不修改仓库或 package 可见性、权限和安全设置。首次创建 GHCR package 按 GitHub 默认 private；既有 package 必须 private 且关联本仓库，否则停止。API/registry 权限不足直接失败，不切换凭据。

## 确切源码与镜像

版本 tag 可以是轻量或带注释 tag，须解析到 main 历史中的提交。发布要求该确切 SHA 的当前 CI workflow main push 最新 run/attempt 成功，且其中 `quality-gate` job 成功；旧绿、PR run、其他仓库/workflow、进行中或已失败的新 attempt 均不放行。版本若指向尚未有 main CI 证据的提交，则等待 CI 完成后重新运行；不以相邻提交代替。

构建接口为 context `.`、根 `Dockerfile`、target `runtime`，由镜像 owner 维护。工作流 checkout 到 `source/`，因此 Action 输入写作 context `source` / file `source/Dockerfile`。构建后执行 `python3 scripts/check_docker_image.py IMAGE`，验证真实 HTTP/静态资源/API/健康、只读和非 root 边界、SIGTERM、SQLite 重开及镜像内容。不重复运行主 CI 全套业务测试，也不削弱现有 quality-gate。

只有验证成功的待发布镜像才以 `docker save` 上传。不可变 artifact ID、当前 run/attempt、源码/workflow SHA、image ID、archive/manifest SHA-256 共同绑定；publisher 只下载当前 run 的确切 ID，重新验证文件集合和摘要。它只从 workflow SHA checkout 发布 helper，不执行 artifact 中脚本或候选镜像业务代码，不重新构建镜像。发布前和 `latest` 更新前都重新核对版本、tag、确切 SHA CI。

## 版本、重试与并发

版本 tag 保持不可变：若已有同版本，仅当远端单平台 manifest 的 config digest 与本次测试 image ID 相同才视为幂等成功，否则拒绝覆盖。先推版本并拉取确切 digest 验证，再考虑 `latest`。prerelease 永不更新 `latest`；stable 只允许向更高版本推进，旧版本重跑不降级。既有 latest 必须有唯一可识别的 stable 版本关联，无法判断则停止。晋级前还会使用同一临时 token 获取仅 pull 的 registry bearer token，先 HEAD 核对已验证版本的确切 digest，再 HEAD 读取 registry 实际 latest；其 digest 必须与 GitHub Packages metadata 的 latest 关联一致。认证、限流、网络、重定向、无效 descriptor 或两者不一致均 fail closed，不写 latest。只有同一 bearer 已能读取已知版本且 latest 明确返回 404，同时 metadata 也无 latest，才允许创建首次 latest；不将错误当作 tag 不存在。

所有本工作流发布 job 使用同一 concurrency group，且不取消运行中的发布。GitHub concurrency 不保证 FIFO，最多保留一个 pending，新的 pending 可能替换旧 pending；被取消的等待任务没有发布。外部发布者不受此锁保护，应由维护者避免并发改写本 package 的 tag。registry HEAD 与最终 alias 写入不构成 CAS；本工作流的单发布锁保证自己的发布串行，无法保证外部写入者在检查后的竞争。

最终别名写入之前的失败不更新 latest。网络中断或最终别名写入后的验证失败可能已经产生远端结果，须先检查 registry digest 与 run summary 再重试，不能承诺网络错误下跨版本/别名的原子事务。版本推送成功、后续失败时保留版本供核对，不删除 package/version。

成功产出 digest、源码 SHA、CI run/attempt、image ID 与 latest 结果到 Actions summary，并保存 release-notes artifact；不申请 contents 写权限去编辑 Release。使用不可变 `IMAGE@sha256:...` 拉取。整个流程不创建 tag/Release，不触发真实服务器部署。

## 验证边界

本地可运行 `python3 -m unittest discover -s tests -v`、`python3 scripts/check_repository.py` 和 actionlint；策略测试使用合成事件/API/制品，不访问真实密钥或推送 registry。真实 Docker 构建与 smoke 必须由 PR 的 `image-build-test` 验证。首次 live 发布、GITHUB_TOKEN 对 private GHCR metadata 的实际访问及 registry 晋级仍需经授权版本发布验证；不能以 mock 通过替代。若关联/权限不满足，失败后由维护者决定后续，不自动改设置或新增 token。

参考：[GitHub 发布 Docker 镜像](https://docs.github.com/en/actions/tutorials/publish-packages/publish-docker-images)、[GHCR 权限与默认可见性](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)、[Release 触发](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#release)、[Docker 单 manifest 晋级](https://docs.docker.com/reference/cli/docker/buildx/imagetools/create/)。

Registry 读取依据：[OCI manifest 存在与 digest 检查](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#checking-if-content-exists-in-the-registry)、[Registry bearer 认证协议](https://distribution.github.io/distribution/spec/auth/token/)。
