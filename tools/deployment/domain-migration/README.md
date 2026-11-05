# CoFleet 旧域名迁移运维工具

仅用于已经授权的自有 CoFleet 实例，使用懒猫官方 SDK 的 PackageManager API。它不是 Manager 的 HTTP API，不进入 LPK，不可暴露给应用页面或接受任意客户端身份。

在受信开发工具容器中通过平台 API gateway 执行，stdin 接收最多 4096 字节 JSON：`owner` 必须来自当前已认证用户的 `/api/whoami`，不得猜测或伪造其他用户。服务端查询结果必须确认该用户确为 CoFleet 实例 owner。

- `action=query`：只查询 `xyz.mxue.lzc.app.cofleet` 的部署与 `test` 基础域是否空闲。
- `action=rename`：只允许将该实例的 `cofleet-test` 改为 `test`，其他当前值和目标冲突均拒绝。已迁移则幂等返回。
- `action=rollback`：只允许 `test` 回到 `cofleet-test`。部署配置的其他字段保持不变。

编译：`CGO_ENABLED=0 go build -o domain-migration .`。不要将含 owner 的私有请求、平台凭据或 state 备份提交仓库。使用后删除远端临时二进制。

先完成状态备份并安装支持 `--access-prefix-domain` 的版本，再改平台部署基础域并更新部署。迁移后必须验证旧管理／Agent URL、入口链路、资源策略、工作区卷与发布版本。任何一步失败，执行 rollback 并重新安装备份 LPK。回滚会撤销内存授权，需要重新打开入口。

适用目标和当次验证结果见 `docs/access-integration.md`。此工具没有验证任意域名迁移，也不支持其他包或多实例自动选择。
