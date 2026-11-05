# 0.7.2：将应用访问整合到原工作台

记录日期：2026-11-05（Asia/Shanghai）。自动前缀入口、明确的应用信任确认、浏览器绑定授权、应用登录与 WebSocket 现已整合到原 CoFleet 包和原工作台。用户不需要切换到独立验证工作台。

## 域名兼容

在本轮目标 lzcos 1.6.2+fix.4／lzc-cli 2.0.9 上，含连字符的原基础标签不能叠加前缀。通过官方 PackageManager `ChangeDeployCfg` 将原 CoFleet 实例的**平台基础标签**由 `cofleet-test` 调整为当时空闲的 `test`；原包 ID、实例、var 目录和 Agent 配置保留。

| 用途 | 迁移后 |
| --- | --- |
| 用户已有管理地址 | `https://cofleet-test.<微服根域>/`，保持不变 |
| 现有 Agent 连接地址 | 保持 `cofleet-test.<微服根域>`；它现在是平台基础域上的合法前缀 |
| 自动应用入口 | `https://e-<稳定ID>-test.<微服根域>/` |
| 平台启动器打开基础域 | `https://test.<微服根域>/` 返回 303 到旧管理地址 |
| 新安装默认基础标签 | `cofleet`，直接使用无连字符标签，不需要旧实例迁移 |

Manager 新增 `--access-prefix-domain`，区分对外管理 Host 和平台实际前缀基础域。它只能等于管理 Host，或为该管理 Host 所属的已配置平台基础域；其他配置拒绝启动。主机和来源校验继续使用固定的管理域，不用请求 Host 生成任意管理回跳。

平台基础域只有根页面跳转，不能访问管理 API。未知 Host 仍返回 404，应用 Host 只代理对应应用；管理 Cookie 仍限定于原管理 Host。应用访问依旧要求平台真实身份、入口所有权和 host-only 访问会话。

启动脚本自动处理此受限旧部署兼容；其他带连字符且未迁移的部署仍不悄悄启用错误前缀。新实例使用 `cofleet`。迁移前检查冲突、当前域和实例 owner，不能任意占用其他应用域名。运维工具见 [tools/deployment/domain-migration](../tools/deployment/domain-migration)。

## 功能与数据

- 在原“应用入口”中发现容器、查看日志、确认信任、创建持久入口并打开。
- 授权、防重放、禁用／删除、管理退出、活跃流撤销沿用 0.7.1 验证能力；本轮不扩展 Docker 生命周期控制。
- 本机 Agent 更新为 0.7.2，复用原 data-dir、设备身份、工作区镜像与启动参数；没有执行重新 init。
- 工作区卷、项目路径、资源预留、调度能力、当前／上一发布版本与镜像均核对保留。
- 独立验证节点不再是访问依赖，原验证包及其专用测试数据保留供回溯，未迁移为用户项目。

本轮开始时两台设备均离线。本机恢复后已通过原地址重连；Windows 设备未在本轮上线，未修改其配置。旧地址兼容另由真实本机连接和模拟 Agent 协议回归共同验证，不能声称已做 Windows 实机重连测试。管理中心重启后的设备目录仍按现有内存目录机制由 Agent 重新上报，离线设备的本机配置和身份没有被删除。

## 验证

- 31 项 Rust 单元测试、6 项协议集成测试通过。新增兼容场景包含旧管理 Host、Agent WebSocket、授权、单次票据、自动前缀生成、基础域跳转与非管理 Host 的 API 拒绝。
- Clippy、原管理界面回归通过。
- 原地址上的真实闭环 17 项通过：信任确认、自动前缀 TLS／平台身份、浏览器授权、HTTP／health、Cookie 隔离、应用 Host 不暴露管理 API、禁用拒绝、新授权、退出撤销、应用 Cookie 登录、Authorization 转发、身份与网关 Cookie 过滤、WebSocket echo 及退出关闭活跃 WebSocket等。
- 原地址的编辑器、项目预览和已发布应用访问通过；HTML、CSS、JavaScript 与本次源码一致，匿名设备访问被拒绝。
- 实际平台基础域根页面返回 303 到旧管理地址；原 Agent 的 manager_url 未修改。

复现闭环：`COFLEET_ACCESS_VERIFY_OUTPUT_DIR=.local/release-0.7.2/ node tools/m01/live.mjs`（需要 Playwright、已登录的懒猫客户端、原测试容器及本机 Agent 在线）。脚本默认使用当前原管理地址，不再需要指定独立工作台 URL。原始报告及 state／LPK 备份仅在 `.local/release-0.7.2/`；脱敏汇总见 [access-integration-results.json](access-integration-results.json)，校准来源与摘要见 [校准记录](deployment/access-integration-calibration.json)。

## 回滚与限制

迁移前备份了旧 LPK、平台域名、设备／负载快照及 CoFleet var 数据。发生兼容失败时，运维工具只将该实例基础域改回 `cofleet-test`，再安装备份 LPK；不重建节点或删容器。本轮实际验证了迁移和旧地址兼容，没有在成功后额外执行回滚演练。

模式仍仅面向用户主动添加的受信任应用，不承诺共享父域普通 Cookie 的完整隔离。第二真实账号、平台端退出和长期负载验收继续保留为未完成项，见 [M0.1](m01-access.md)。
