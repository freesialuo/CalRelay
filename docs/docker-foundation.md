# Docker 交付基础（#105 / #38）

本叶子提供真实 Rust 标准 binary + 构建 TS 静态页面的单镜像基础，现已同步正式 source/planner/delivery 三工作器装配。它不代表 #75/#78 的真实账号、完整 UI 业务旅程或外部投递闭环已经验收。恢复监督器、持续 unhealthy 后的外部终止、Engine stop/kill/manual-stop 竞争、arm64 实际运行、扫描/SBOM 和备份升级演练仍由 #38 跟踪。

## 构建与配置

```sh
docker build --target runtime --tag calrelay:local .
cp examples/docker-deployment.json deployment.json
mkdir -p secrets
# 按下文准备32字节 master-key、Turnstile secret，并替换全部合成身份配置
docker compose config --quiet
docker compose up -d
```

示例 JSON 显式选用容器监听 `0.0.0.0:3000`，Compose 只发布到宿主 loopback；它包含完整但全部为合成值的 admin 信任元数据与实际 KeyRing 文件路径。部署者必须替换 Access team/audience/subject、公开 HTTPS origin/hostname/sitekey 等值，按自己的密钥流程准备恰好32个原始字节的 master-key 和独立 Turnstile secret，分别保存在只读 secrets/calrelay-master-v1 与 secrets/turnstile；没有内置 key，缺失或错误应启动失败。示例不会赋予真实身份或绕过鉴权。该地址/端口只是部署示例，不是项目默认值或新的接口契约。监听只能由 `--deployment-config` JSON 配置，不能使用 `CALRELAY_BIND` 或其他监听 env。`CALRELAY_DATABASE=/data/calrelay.db` 是现有路径 env；业务来源、周期、规则和输出仍由管理 API/SQLite 决定。

实际管理部署须在部署文件配置 [已有 admin bootstrap](admin-auth.md)，通过可信代理提供 HTTPS/Access，令 bootstrap hostname 与公开管理页面域名一致。管理入口是 `/admin/`，不能通过放宽 CSP、mock JWT 或 harness 替代真实身份。Turnstile secret 只读挂在 `/run/secrets/`，文件及目录须让 UID/GID 65532 可读/可遍历；应用不修改权限。保留只读文件挂载，部署者按自己的密钥管理流程准备文件，不把内容放 env、build arg 或镜像。KeyRing/Bark provisioning 复用已合并的 [正式凭据 CLI](credential-bootstrap.md)：在容器里执行 calrelay credential provision-bark/rotate-bark，使用同一部署 JSON、持久卷与只读 secret 文件；仅 credential_id 回填管理 UI，不手改 DB 或复制 secret 进镜像。命令不会调用 Bark/Cloudflare；真实凭据值只由部署者的文件管理流程提供。

Compose 的 `deployment.json` 与 `secrets` bind 必须预先存在（`create_host_path: false`），配置错误应明确失败。不要把合成身份配置当作真实生产账号。可把整个服务接入受控内部代理网络；不要直接公开管理端口、Docker socket 或任意来源 CORS。更改示例端口时同时修改 JSON bind 与 Compose 端口映射；不新增监听 env。

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

脚本只创建带随机本任务前缀的容器与 named volumes，使用完整实际 admin/credentials 配置、合成身份/32字节 key 和 secret canary，最终清理自己的容器/卷。验证真实首页/资产/API、CLI/live、实际 readiness、Docker healthy 与无就绪误重启、非 root/只读挂载、SIGTERM 干净退出、正式 CLI 创建/重开解密旋转 Bark 凭据、SQLite 完整性与历史持久，以及导出根目录无 Node/harness/源码/secret canary。脚本不模拟/禁用鉴权，验证缺失管理身份真实拒绝，不 seed 业务源、不发送 Bark、不连接生产账号。新建且非恢复暂停的数据卷必须在有限启动期限内达到 ready=200，重开后亦然；503 不能冒充成功。该值由正式三工作器的实际操作/HTTP/DB健康决定，不注入模拟进度。恢复暂停造成 ready=false 属于另一个显式恢复场景，live仍应正常，不能为通过此 fresh-start smoke 自动 resume/cutoff。此停止测试不替代外部恢复和 stop/kill API 竞争实验。

镜像 build/publish workflow 由独立 CD owner 统一维护，本叶子不再另加镜像 job 或重复 gate wiring。CD owner 可在隔离 runner 构建 runtime target 后执行同一脚本，且 PR 不 push、不注入生产 secrets。实际 build/run 与当前 head 检查以统一 workflow 的真实结果为准；本地常规多阶段 Dockerfile 网络构建尚未完成，arm64 亦未验证，index 存在 arm64 不等于已验收。

本地 Cloud 环境已读取 Engine 28.4.0、Compose 2.40.3、buildx 0.31.0。2026-10-25 首次 build 在 npm ci 阶段受 BuildKit 容器 DNS 限制；独立 5 秒 registry 请求报告 EAI_AGAIN，构建已取消。没有修改 daemon/DNS/网络或凭据；这次常规网络构建未完成，不能当作完整构建成功。后续离线镜像运行与标准 runner 的成功证据分别见下。buildx 默认 home cache 只读时可使用工作区专用空配置路径，不复制或修改任何账号凭据。


历史离线验证（#112 合并前的未装配 worker 基线）：在相同固定 Node/Rust 基础镜像中，以禁网容器和只读挂载的专用公开依赖缓存执行 `npm ci --offline --ignore-scripts`、真实 typecheck/build/bundle canary，以及 `cargo build --release --offline --locked -p calendar-server --bin calendar-server --no-default-features --features production-web`。缓存只挂 npm `_cacache` 和 Cargo `registry`，不挂用户凭据或工具配置。将该实际编译 payload 交给 Dockerfile 未改动的最终 `runtime` 阶段，本地镜像 `sha256:2cd06b1baa28387a05dc902e5a45f709751ac3f9ffcca201fbad07ffc1e0e119` 通过当时版本的 smoke：真实 HTTP/管理拒绝/健康、非 root/只读挂载、30秒内 SIGTERM、SQLite/正式凭据 CLI 重开旋转、CA/时区与镜像内容。当时实际 ready=503，只反映该旧基线没有装配生产 worker；保留这个真实历史结果，不作为当前正常服务的通过条件。

当前生产基线（#112 合并后，`main@0067635`）已注册真实 source/planner/delivery 三工作器。新建、非恢复暂停的数据卷和正常持久重开必须达到 ready=200；本地正式三工作器镜像 `sha256:4b12e1495f388b9788d9382533eeba898aaf7a46102ec43dc364c3a3c742f655` 已通过收紧后的 smoke：启动/重开 ready=200、live/ready CLI、缺少 admin/credentials 时在创建 DB 前拒绝、只读挂载、SIGTERM 及凭据/SQLite 持久化。统一 CD 的[标准多阶段构建与实际镜像 smoke（run 40189519609）](https://github.com/freesialuo/CalRelay/actions/runs/40189519609)亦成功，publish 跳过；此处仅记录已确认结果，纯文档修订不使这些对应代码/构建输入的证据失效。

恢复暂停是独立状态：可以 ready=false，但 live 应保持健康，不能自动 resume 或改变 cutoff。当前 smoke 使用新建且非暂停的数据卷；显式备份恢复后的 image pause 演练尚未执行，不能用上述 fresh-start 成功替代它，也不能把历史未装配 worker 的 503 当作当前正常启动预期。

这项证据验证真实编译与 runtime 容器，**不把离线分步构建称为常规多阶段 Dockerfile 已在本地完整通过**。后者的标准网络构建/运行由统一 CD runner 验证，结果以对应提交的实际检查为准。首轮本地 smoke 还检出了测试输入默认0600、docker cp后root所有导致非root不可读的问题；仅合成测试文件显式0444，生产权限和鉴权保持严格，失败时输出有界脱敏诊断。
