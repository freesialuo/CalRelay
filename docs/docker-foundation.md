# Docker 交付基础（#105 / #38）

本叶子提供真实 Rust 标准 binary + 构建 TS 静态页面的单镜像基础。它不代表 #75/#78 的生产业务循环、首次凭据初始化或完整 UI 旅程已经验收。恢复监督器、持续 unhealthy 后的外部终止、Engine stop/kill/manual-stop 竞争、arm64 实际运行、扫描/SBOM 和备份升级演练仍由 #38 跟踪。

## 构建与配置

```sh
docker build --target runtime --tag calrelay:local .
cp examples/docker-deployment.json deployment.json
mkdir -p secrets
docker compose config --quiet
docker compose up -d
```

示例 JSON 只显式选用容器监听 `0.0.0.0:3000`，Compose 只发布到宿主 loopback；它不启用管理员身份。该地址/端口只是部署示例，不是项目默认值或新的接口契约。监听只能由 `--deployment-config` JSON 配置，不能使用 `CALRELAY_BIND` 或其他监听 env。`CALRELAY_DATABASE=/data/calrelay.db` 是现有路径 env；业务来源、周期、规则和输出仍由管理 API/SQLite 决定。

实际管理部署须在部署文件配置 [已有 admin bootstrap](admin-auth.md)，通过可信代理提供 HTTPS/Access，令 bootstrap hostname 与公开管理页面域名一致。管理入口是 `/admin/`，不能通过放宽 CSP、mock JWT 或 harness 替代真实身份。Turnstile secret 只读挂在 `/run/secrets/`，文件及目录须让 UID/GID 65532 可读/可遍历；应用不修改权限。保留只读文件挂载，部署者按自己的密钥管理流程准备文件，不把内容放 env、build arg 或镜像。KeyRing/Bark provisioning 的最终部署入口仍等 #99 实际契约，不在此自造。

Compose 的 `deployment.json` 与 `secrets` bind 必须预先存在（`create_host_path: false`），配置错误应明确失败。不要把示例无管理员模式用于生产管理。可把整个服务接入受控内部代理网络；不要直接公开管理端口、Docker socket 或任意来源 CORS。更改示例端口时同时修改 JSON bind 与 Compose 端口映射；不新增监听 env。

## 构建与运行边界

CD 构建接口：context 为仓库根 `.`，file 为根 `Dockerfile`，最终 target 为 `runtime`（也是默认最后阶段）；中间阶段为 `web` / `rust`。无 secret/build arg、无可开启 harness 的参数，Rust feature 固定在 Dockerfile 内。PR 构建不 push，发布 workflow 由独立流程 owner 维护，本叶子不执行首次发布。

- Rust 1.99.1、Node 24.19.0、distroless cc-debian12 nonroot 使用固定多架构 index digest；Cargo.lock/npm lockfile 锁定依赖。常规依赖更新通过 PR 更新 digest 与验证记录，不跟随浮动 tag。此基础不承诺跨不同 CPU/编译环境的逐字节相同结果。
- Node 阶段真实 typecheck/build/bundle canary；Rust 只构建 `calendar-server --no-default-features --features production-web`，未启用 `test-harness`，最终仅复制 strip 后 binary 与空 `/data`。运行不需要 Node、编译器、shell、dist 目录或开发服务器。
- `.dockerignore` 是源码输入 allowlist，不发送部署文件、运行 DB、secret 目录、环境文件、前端测试或 admin harness；不要在受信任源码文件中存放凭据。配置/secret 通过运行挂载提供，不能 COPY 进任一构建阶段。
- `/data` 由 UID/GID 65532 拥有，整个目录使用本地 named volume，包含 SQLite DB/WAL/SHM。不要只持久化 `.db`，不要以 NFS/多实例共享 SQLite 作为 HA。已有 bind 数据目录需部署者自行准备正确权限，应用不以 root 修权限。
- Compose 根目录只读、cap_drop ALL、no-new-privileges、128 PID、512 MiB、1 CPU、16 MiB `/tmp` tmpfs；镜像包含 CA 根证书与时区数据，时间同步由宿主负责。资源值是示例部署预算，调整后需重新验收实际负载。
- exec ENTRYPOINT 让标准服务直接接收 SIGTERM。Compose 停止宽限 30 秒；#71 当前默认关闭最多两个 10 秒窗口用于排空及取消/回滚，留有调度余量。完全卡死或自定义更长预算的部署须另行核对停止预算，不能宣称一定干净退出。

## 健康与恢复

镜像 HEALTHCHECK 直接执行 `calrelay healthcheck live --deployment-config /run/calrelay/deployment.json --timeout-ms 2000`，不依赖 shell/curl。每 15 秒、探针上限 3 秒、启动宽限 60 秒、连续 3 次失败标记 unhealthy。CLI 读取同一部署文档的 bind，wildcard 映射本机 loopback；不得另启 runtime、打开 DB 或修改 pause/cutoff。

`restart: unless-stopped` 响应进程退出；HEALTHCHECK 不会自动 kill/restart。没有 `probe || kill 1`、autoheal sidecar、恢复权限或 Docker socket。ready=false、迁移、restore pause、来源/Bark 离线均不足以触发终止。核心只报告真实 live/ready，持续核心失败后的外部恢复机制仍未选型/验收；手动停止保持停止的完整竞争验收仍待 #38。详见 [运行与恢复](operations.md#8-docker-恢复语义与验收)。

## 实际验收

```sh
python3 scripts/check_docker_image.py calrelay:local
```

脚本只创建带随机本任务前缀的容器与 named volumes，使用合成 bind JSON 和 secret canary，最终清理自己的容器/卷。验证真实首页/资产/API、CLI/live、实际 readiness、Docker healthy 与无就绪误重启、非 root/只读挂载、SIGTERM 干净退出、SQLite 迁移/完整性重开持久，以及导出根目录无 Node/harness/源码/secret canary。脚本不模拟鉴权，不 seed 业务源、不发送 Bark、不连接生产账号。当前 runtime 未组合时应观察 ready=503；后续真实组合可为 200，测试不会伪造 readiness。此停止测试不替代外部恢复和 stop/kill API 竞争实验。

CI 的 docker job 使用隔离 GitHub runner，执行同一锁定构建和实际 Engine 脚本，并加入 quality-gate；不登录 registry、不推镜像、不注入生产 secrets。当前仅验证 runner 原生 amd64；index 存在 arm64 不等于 arm64 构建/运行已验证。

本地 Cloud 环境已读取 Engine 28.4.0、Compose 2.40.3、buildx 0.31.0。2026-10-25 首次 build 在 npm ci 阶段受 BuildKit 容器 DNS 限制；独立 5 秒 registry 请求报告 EAI_AGAIN，构建已取消。没有修改 daemon/DNS/网络或凭据，不能声称本地镜像运行通过；真实受控 CI 结果以当前 PR head Checks 为准。buildx 默认 home cache 只读时可使用工作区专用空配置路径，不复制或修改任何账号凭据。
