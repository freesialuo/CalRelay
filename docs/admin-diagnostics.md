# 只读管理诊断

在网页“诊断”选择来源，再读取持久诊断。来源选项每页最多100条；诊断每次返回该来源最近创建的20条投递任务，有更多记录时明确提示，不是完整审计或自动监控。刷新、切换来源、返回页面或会话失效会清除旧结果。读取不会触发同步、规划或投递。

`GET /api/admin/v1/sources/{source_id}/diagnostics` 复用现有 Access + 管理会话 guard，响应 `Cache-Control: no-store`。source/feed capability 不提供管理权限，GET 不消耗写 nonce；本功能没有写接口。来源、快照及投递投影在同一 SQLite 读事务内获取，查询出错时失败，不用空数据代替。

- `last_verified_at` 是最近成功同步或条件读取校验的时间，不是“最近尝试成功”。配置变更可能按已有配置逻辑清空它；从未校验与清空后的未知状态均显示无记录。
- `snapshot_revision` 无值表示没有可用的已提交快照版本。`freshness` 使用既有配置判定：静态来源、无 SLA 的 push-only 不显示假轮询；有 SLA 的来源区分未校验、新鲜、陈旧和未来时间。启用/禁用/退休状态单独展示，陈旧不等于核心故障。
- 逐来源最近错误原因没有持久查询支持，因此明确显示不可查询；不猜测网络、凭据或配置哪个失败，也不展示外部原始响应。
- 投递显示当前 job state、最近 attempt outcome 和时间，兼容仍在途、结果未知、重试及晚到 accepted 的不同状态。`accepted`/`accepted_at` 表示上游接收，不代表设备显示；无任务不能证明规则完成规划。`next_attempt_at` 是持久调度记录，只有 retry_wait 才标为下次重试，不承诺精确执行时刻。
- 仅输出来源/任务/规则/输出标识、枚举和时间，不输出事件正文、endpoint、credential handle、lease token、request ID、feed URL/token 或外部错误正文。所有文本通过 DOM textContent 渲染，未可信或超限响应不显示为成功。

验证范围：真实 HTTPS 浏览器登录、guard、SQLite 和读取 API；投递状态由测试专用 SQLite 夹具构造，不发送真实 Bark，也不代表生产端到端互操作已完成。#77/#35 的其余验收继续保留。
