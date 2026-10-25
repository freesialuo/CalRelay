# 本地 Bark 凭据初始化与 runtime KeyRing（#99）

本叶子复用 #26 的 KeyRing/SecurityStore/AEAD 和 #93 的 DeploymentConfig。只实现 Bark 本地初始化、CAS替换/轮换、统一只读 provider；无新迁移、凭据HTTP API、默认密钥或真实账号操作。业务配置继续只引用#97的credential_id；不改变来源、ingest/feed token、管理认证或监听契约。

## 部署契约

```json
{"bind":"127.0.0.1:8080","credentials":{"current_key_version":1,"key_files":{"1":"/run/secrets/calrelay-master-v1"}}}
```

credentials可省略；提供时两个字段必填，未知/重复字段、null、非正/非规范版本号、重复版本、超过16个key或相对路径均拒绝。每个master-key文件恰好32个原始字节，不接受编码或换行。当前版本必须在映射中；保留旧版本直到所有记录和备份都已完成轮换。没有默认测试key或自动生成key。

配置解析仅读取部署JSON并验证形状/路径，不打开master-key/Turnstile文件或数据库。只有标准server的initialize和明确的credential命令加载KeyRing；健康/诊断CLI应只读取DeploymentConfig.server，不调用initialize。bind仍只来自部署JSON，CALRELAY_BIND继续拒绝，数据库沿用CALRELAY_DATABASE。

初始化在监听前完成：加载配置的KeyRing和admin verifier，迁移数据库，装配共享Services/provider与管理guard，之后才bind。错误固定脱敏；无KeyRing时凭据provider返回MissingKey，不回退明文。恢复pause/cutoff不因装配或CLI解除。

## 显式本地命令

```text
calendar-server credential provision-bark --deployment-config FILE --credential-id ID --secret-file FILE
calendar-server credential provision-bark --deployment-config FILE --credential-id ID --secret-file FILE --expected-revision N
calendar-server credential rotate-bark --deployment-config FILE --credential-id ID --expected-revision N
```

这些命令不启动HTTP、不加载Turnstile secret、不调用Bark/Cloudflare。部署JSON和Bark/master-key文件由部署管理员提供；不用手改数据库或执行生产seed。secret文件路径绝对，值不在argv/env/URL中。ID限定1–128个ASCII字母数字、下划线或连字符，与#97引用契约一致。

无expected-revision仅创建，已存在不覆盖；初始revision为0。替换或轮换必须提供非负revision，CAS失败不写入。只允许kind=bark；purpose绑定与revision/旧密文/nonce/key version一起复核，不用单独kind检查后的无条件更新。轮换读取原值，仅用current key和新nonce重新加密；输出不含原值。

KeyRing在启动时加载，不监视文件变化。轮换应先将新旧key版本都配置并重启服务，再执行CAS轮换；核对所有记录和备份保留计划后，才移除旧key并重启。

成功stdout严格只有JSON，例如 `{"credential_id":"bark-main","revision":0}`。这是本次提交的回执，不是并发写入后的最新状态保证。错误非零退出、stdout为空、stderr固定文本；不打印路径、SQL、密钥或secret。CLI是持有数据库和key文件访问权的本地主机管理操作，不新增远程身份通道。若进程在提交后、回执送达前被终止，结果可能未知；不要将非零退出或回执缺失视为一定未提交，也不要去掉CAS或改用覆盖重试。

## 文件与隐私边界

共享read_secret_file以只读方式、有界读取常规文件；Unix使用O_NOFOLLOW/O_NONBLOCK，拒绝末端symlink、FIFO、目录、不可读/缺失/超限文件。master-key要求32字节，Bark/Turnstile要求1–4096个ASCII graphic字节，拒绝末尾换行。SecretBytes/Zeroizing保护缓冲区的生命周期，错误不附带底层参数。

部署者仍负责可信父目录、文件访问权限和只读secret挂载；代码不修改权限、不证明所有父目录无symlink或实际mount ro属性。KeyRing只在内存中，数据库只存既有认证密文与nonce/版本/revision。特权数据库历史回滚和进程/主机失陷不在隔离保证内；恢复备份须保留对应旧key。

## runtime 构造契约

```rust,ignore
use calendar_application::CredentialProvider;
let server = deployment.initialize().await?;
let provider = server.services.credential_provider()?;
let provider_for_worker = provider.clone();
let secret = provider_for_worker.credential(credential_id, "bark").await?;
```

RuntimeCredentialProvider持有共享Services/KeyRing，实现既有CredentialProvider，按id和expected_kind查询同一SQLite记录并返回对应revision。构造时缺KeyRing即失败；provider不拥有写入/签发/删除能力，不公开密钥或明文序列化。真实Bark adapter只在可信发送边界expose，沿用已有HTTPS POST/超时/错误协议。worker仍通过run_task准入，关闭后pool拒绝新访问；不改变监督/健康或自动恢复策略。

## 验证与范围

真实子进程和临时SQLite验证create-only、CAS替换/并发冲突、purpose拒绝、轮换后移除旧key、重开解密及既有Bark配置引用；文件/配置负例、输出脱敏、parse-only不打开缺失key/DB、失败不创建数据库、SIGTERM/restore状态保持。所有secret/key均为合成测试材料；不进行真实服务调用或用户凭据读取。

有效Red：标准binary原先拒绝credential子命令，合成provision测试观察到预期成功缺失；夹具/编译失败不计为Red。另外去掉purpose谓词的受控mutation会把跨kind替换变为Ok而预期Denied；恢复后通过。最终实际gate以PR记录为准。

实现依据沿用[既有安全实现](credential-security.md)、[认证装配](admin-auth.md)、[Rust OpenOptionsExt](https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html)与[SQLite事务](https://www.sqlite.org/lang_transaction.html)。不自行实现加密原语。
