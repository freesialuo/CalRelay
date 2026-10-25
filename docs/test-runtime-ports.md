# 受控测试 Clock 与 SQLite 生命周期（#115）

这些 typed API 仅在显式 `test-harness` feature 下存在。它们不修改 `admin_harness.rs`、CLI 参数或 fixture 网络映射；这些由 #116 统一接线。标准 binary 始终调用生产装配，无 mock 开关、loopback 豁免或默认 key。生产与真实镜像证据、harness 外部模拟证据分别验收。

## 调用契约

`calendar_server::test_runtime` 导出：

- `HarnessRoot::new(PathBuf)`：专用隔离目录必须是 canonical 绝对路径、名称以 `calrelay-system-` 开头、Unix 当前有效 UID 所有、group/other 无权限。目录、数据库、部署文件、key 文件、clock 文件、snapshot/restore 目标均限制在该根内，拒绝 symlink、`..`、非普通文件/目录和越界。
- `initialize(database, deployment_file, DatabaseMode)`：部署文件只含已有 bind/credentials 契约，bind 必须 loopback:0，不包含 admin；真实 KeyRing 在触碰 DB 前加载。返回持有真实 provider/store 的 Server，调用者继续装配既有真实 guard。不会创建假 credential，handle 由正式 credential CLI 创建。
- `DatabaseMode::Fresh`（默认）专用 `create_new`，已有 DB 一律拒绝；`Reopen` 必须是已有非空常规文件；`Restored` 还必须实际 `paused=true` 且有持久 cutoff。后两种模式不会 resume、重置 cutoff 或清空状态。调用者必须明确选择，不能把失败重试降级为 fresh。
- `backup(database, new_destination)` / `restore(snapshot_directory, new_destination, cutoff)`：复用 storage 原有 WAL-aware snapshot、manifest/校验和、迁移与暂停发布，返回恢复后的 `database.sqlite3` 路径。目的目录必须全新，不覆盖。调用者负责停止/隔离 runtime 后再操作；这里不杀进程、不启动恢复控制器、不自动 resume。

```rust,ignore
let root = HarnessRoot::new(run_directory)?;
let clock = Arc::new(FileBusinessClock::load(&root, clock_file)?);
let server = root.initialize(db, credentials_deployment, DatabaseMode::Fresh).await?;
let server = server.with_admin_site_key(existing_verifier, site_key)?;
let server = server.with_test_runtime_ports_and_clock(existing_runner, existing_output, clock)?;
```

最后一个方法复用同一 source/planner/delivery composition、既有 SourceRunner/PushOutput/Clock 和真实配置查询；#116 只替换外部 transport，不复制循环、planner、store/provider 或 guard。运行时安装仍要求 admin 与实际 credential provider。支持任意满足既有 Clock 契约的 `Arc<C>`；不必使用下面的文件适配器。

## 可控业务时钟

`FileBusinessClock::load(&HarnessRoot, path)` 实现既有 Clock。文件使用 `ClockFrame` JSON：

```json
{"utc_now_ms":1793638800000,"monotonic_offset_ms":0}
```

控制者必须用同一目录中新文件+rename 原子替换。读取有1024字节上限、普通文件/路径检查和10ms有界缓存。UTC 可前后跳；monotonic_offset_ms 在同一进程只能递增（上限365日）。返回的单调值始终还包含真实 Instant elapsed，因此冻结业务 UTC/offset 也不能冻结展开/轮次资源预算。非法后续控制或 monotonic 回退会使实际 worker future 失败/panic，由现有 supervisor 诊断；不会伪造进度或自动重启。

`with_test_runtime_ports_and_clock` 将**同一 Arc Clock**用于三个 worker，并仅在显式测试装配下用于 aggregate freshness、single/aggregate capability expiry、aggregate 管理的业务 expiry 时间。所有原有授权、future/stale 判定、cache/304 最终 fence 仍执行：推进 UTC 后未验证的旧快照仍503，过期 capability 仍404，source verified_at 在 UTC 未来也仍拒绝。没有改写快照时间或豁免 freshness。

JWT/Turnstile、identity/session/nonce 和其他既有管理审计时钟保持真实/原有 AdminState.clock；HTTP/DB probes、supervisor 停滞/停止预算使用真实单调时间。默认或生产装配没有注入值，继续原有真实 UTC 语义。legacy single feed 保持原有策略，不因此新增或删除来源 freshness 规则。需要 producer/security 时间矩阵时使用真实 UTC，除非另有明确的对应时钟契约。

## 当前证据

Rust 回归使用真实 SQLite、实际本地 CLI 合成 handle、真实 KeyRing provider、既有 guard、同一三 worker 和现有同步/规划/投递用例。验证业务 UTC 前/后跳与 real elapsed、未来/过期/stale 在 cache/304 前仍拒绝、304 后 worker/feed 时钟一致、fresh 默认拒绝已有 DB、显式重开保留 accepted/水位、snapshot/restore 保持 pause/cutoff。恢复后推进 clock，三个 worker 仍只报告真实暂停读取：无 fetch/send、水位不延展、live=true/ready=false，不自动 resume。

独立原型 CLI/映射保存在参考分支 `test/115-overlap-reference` 的 cd50331，供 #116 协调复用；该原型不是本 PR 的 CLI 契约，不应整段覆盖 #116。正式 #115 diff 不修改共享 binary 或 fixture 映射。
