# 凭据与机器 token（#26）

`calendar-application` 定义 `SecretBytes`、分类错误与 `CredentialProvider`；`calendar-storage::security` 实现文件密钥加载、AEAD 凭据和持久化机器能力。无 Cloudflare、管理会话、HTTP 路由或真实外部调用；后续 #27/#28/#30/#32/#33 负责装配。

## 加密、加载与轮换

- 使用固定 `chacha20poly1305 0.10.1` 的 XChaCha20-Poly1305；每次写入由操作系统 CSPRNG 产生独立 24 字节 nonce。密文包含 16 字节认证标签，明文长度限 1–65536 字节。
- AAD 是 JSON 元组 `("CalRelay/credential/v1", id, kind, key_version)`，将凭据身份、用途与密钥版本绑定到密文；复制到其他行或篡改元数据均无法解密。SQL 同时禁止身份/类型更新。
- `KeyRing::from_files(current, files)` 从版本到文件路径的映射加载正整数版本；每个文件恰好 32 原始字节，不接受文本编码、换行、缺失或长度错误。读取上限 33 字节。调用者须把文件独立于数据库、以只读 secret 挂载；库读取文件，不负责验证容器挂载标志或部署配置。
- 主密钥、明文包装及随机 token 缓冲在 drop 时由 `zeroize` 清除；`SecretBytes` 不提供 Display/Serialize/Clone，Debug 固定脱敏。调用者仅在可信加密或网络边界通过 `expose()` 取值，并负责不打印、不自行长久复制。
- `create_credential` 新建；`replace_credential` 用预期 revision 原子替换外部凭据值，也可用新值恢复已无法解密的记录；`rotate_credential` 解密旧值再用当前密钥重加密。更新以 revision 及原密文等条件执行 CAS，同时写 nonce、版本、revision 和 rotated_at，失败不部分写入。并发旧 revision 返回 Conflict，不覆盖新值。
- 轮换步骤：独立生成新密钥文件并只读挂载；在 key ring 中保留旧版本、切换 current；按记录的 revision 逐条 rotate；核对所有 `credentials.key_version`，完成备份/恢复密钥保留计划后再移除旧密钥。中断后根据版本和 revision 续做；不自动丢弃旧密钥，也不把整库轮换宣称为单一事务。
- MissingKey、Authentication、KeyFile、Storage 等错误为固定文本，不携带密钥、明文、SQL 参数、底层异常或文件路径。解密失败不返回部分明文。

迁移 `0005` 建立 credentials、api_tokens、feed_subscriptions，给 sources 添加凭据外键。outputs 已有 credential_id，通过插入/更新及被引用凭据删除触发器保护引用，避免重建与投递历史关联的表。历史非空 outputs.credential_id 在旧版本尚无 credentials 可引用，迁移会拒绝该悬空引用并回滚，保留原表、配置和迁移历史；维护者须先调查并显式修复历史配置，不自动清空引用。

## token 边界

- 每次签发生成 32 随机字节，以 64 个小写十六进制字符返回一次；只落库 SHA-256 摘要。摘要输入有版本与 `api`/`feed` 用途前缀，无明文 token、主密钥或管理员身份信息。
- API token 绑定存在的 source_id，分别授予 Upsert/Delete，至少一项；鉴权同时验证摘要、来源、动作、来源 enabled、revoked_at 和 expires_at。到期等号即失效，时间由调用者传入 UTC Unix 毫秒。
- 订阅 token 绑定 public_id 和单一来源，只返回已授权来源，不提供写入能力；验证 token 与 public_id 后才取得来源。历史 source-only 鉴权 helper 只接受字面空对象 scope_json，其他范围显式拒绝；[#32 独立 feed 边界](readonly-feed.md)返回完整已验证 policy，实现有限字段/日历过滤、权限先于缓存、ETag 与路由。
- `rotate_feed_token` 使用订阅 revision CAS 更换摘要，旧 token 立即失效，保留来源、期限与范围，撤销后不能借轮换复活。API token 更新通过签发新的最小权限 token 并撤销旧记录完成；不复用订阅或管理会话。
- 撤销持久化且幂等，不因 UTC 回跳恢复。鉴权结果只是当次检查结果，敏感业务提交仍需在业务边界处理配置/权限变更竞争，不能把长期缓存的鉴权结果当成永不过期授权。

## 验收与实际证据

| AC | 可观测结果与测试（tests/security.rs） | 层级 |
| --- | --- | --- |
| AC1 | 文件长度/版本拒绝，真实数据库重开解密，密钥轮换移除旧 key 后仍可读，替换/CAS 冲突，写失败保持旧密文、revision 与时间 | 验收/集成 |
| AC1 | v4 升级保留输出，悬空历史引用导致整个 v5 回滚，来源和输出引用约束 | 迁移集成/故障注入 |
| AC2 | 来源/动作/到期等号/撤销/禁用拒绝，重开仍有效；API 与订阅隔离，订阅并发轮换仅一个成功，旧 token 失效 | 安全负例/并发集成 |
| AC3 | 逐项密文、nonce、身份、类型、版本篡改与错误密钥拒绝；同 key material 不同版本仍受 AAD 保护；固定诊断和秘密 Debug 脱敏 | 安全负例 |

有效 Red：先运行占位错误返回实现，5 项初始安全测试中 4 失败、1 通过；不存在编译或夹具错误。Green 后补齐迁移/故障/禁用与独立 AAD 负例。受控将来源谓词替换为恒可接受条件，跨来源测试观察到 `Ok("t")` 而预期 Denied，成功检出 mutation 后恢复生产实现。

不宣称形式证明、长时 fuzz 或完整数据库防篡改。AEAD 保护凭据密文及绑定身份，无法阻止攻击者回滚整个合法历史数据库；持有主密钥、进程内存或可信管理员权限者不在此隔离边界内。数据库内事件及普通配置未由本项整体加密。

## 实现参考

通过 Exa 查询并采用 [RustCrypto 0.10.1 公共源码与用法](https://docs.rs/chacha20poly1305/0.10.1/x86_64-unknown-linux-gnu/src/chacha20poly1305/lib.rs.html) 和 [AEAD 类型文档](https://docs.rs/chacha20poly1305/0.10.1/x86_64-unknown-linux-gnu/chacha20poly1305/struct.ChaChaPoly1305.html)，核对 XNonce 的 192 bit 长度、随机 nonce、Payload AAD 及认证失败。未自行实现加密原语。
