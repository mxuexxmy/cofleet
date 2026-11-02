# 懒猫微服部署验证

验证日期：2026-11-02（Asia/Shanghai）。

## 已部署实例

- 微服：一台已验证的 x86_64 设备，实例名称已脱敏。
- 系统：`lzcos v1.6.2+fix.4`，x86_64。
- CLI：`lzc-cli 2.0.9`，通过现有 `mise exec node@26.8.2 -- lzc-cli` 调用，未修改系统 Node 配置。
- 包 ID：`cloud.lazycat.app.cofleet.test`，版本 `0.1.0`。
- 入口格式：`https://cofleet-test.<YOUR-BOX>.heiyu.space`，实际实例地址不在公开仓库中发布。
- 本地包：`dist/cofleet-test-0.1.0.lpk`，约 2.1 MB；SHA-256：`b05d080b0275f4cf6b01d0d11c42dc421ea48582f35b0799f055f786a0b32d26`。
- 容器：`cloudlazycatappcofleettest-app-1`，应用健康状态正常。

测试版保持安装与运行；临时 Agent 已停止并删除临时配置，没有安装本机开机服务。页面可能显示验证节点离线，这是测试结束后的预期结果。

## 真实链路结果

脱敏结果见 [verification.example.json](verification.example.json)。原始验证记录仅保存在本机，已从 Git 排除。

| 检查 | 结果 |
| --- | --- |
| 本机客户端导出会话，访问平台保护的 `/api/whoami` | 通过 |
| 匿名访问设备 API | 平台返回 307 登录跳转 |
| 无 Cookie、伪造 `X-HC-User-ID` | 同样被平台拦截，返回 307 |
| Rust Agent 自动读取本机登录、建立真实 WSS 连接 | 通过 |
| 本地暂停/恢复，通过长连接同步到微服 | 通过 |
| 中枢下行请求、Agent 带请求 ID 应答 | 通过 |
| 重启测试容器后 Agent 自动重连、设备 ID 保持 | 通过 |
| 停止 Agent 后节点转为离线 | 通过 |

服务端快照：lzcinit 与 manager 的运行 UID 均为 1000，manager 单线程，监听地址为容器内 `127.0.0.1:9760`。因此可信代理限定为同容器回环对端，不开放直接网络访问中枢的端口。

运行时瞬时采样中 manager RSS 为 2164 KiB，整个应用容器工作集约 2.371 MiB，CPU 快照 0.19%。这不是长期性能基线，也不包含电脑端、浏览器或任务负载。

## 规范校准与兼容差异

已执行懒猫开发 skill 的在线校准流程，核对 package、manifest、build、LPK 格式、身份 Header、HTTP 路由六篇官方原文；结果为 success，来源与正文 SHA-256 见 [calibration.json](calibration.json)。目标版本为 `1.6.2+fix.4 / 2.0.9`，未固定官方文档提交。

实际发现并处理：

1. CLI lint 将 `application.run_as` 报为未知字段，但官方规范声明 `lzcos >=1.6.0` 支持。保留配置并在实际容器中确认 UID 1000 生效。
2. 目标安装器拒绝 `unsupported_platforms: [linux/arm64]`，报 `invalid platform`。删除该可选字段，包描述明确 x86_64；本包仅验证在当前 x86_64 微服运行，不能据此分发到 ARM 设备。
3. 测试包未指定图标，CLI 构建有图标提示；这是内部验证包，未提交应用商店，也未宣告商店审核完成。

静态二进制使用 Rust musl target 构建。本机未安装系统 musl 开发包，构建时将官方 Arch 仓库的 musl 包解包到被忽略的 `.local/toolchains/`，并使用本地 wrapper；没有修改 `/usr` 或系统配置。

## 重复构建与验证

需要 Rust `x86_64-unknown-linux-musl` target，以及 `musl-gcc`（或通过 `CC_x86_64_unknown_linux_musl` 指定兼容编译器）。

```sh
# 本机既有 CLI 所在的 Node 环境
mise exec node@26.8.2 -- lzc-cli project build -o dist/cofleet-test-0.1.0.lpk
mise exec node@26.8.2 -- lzc-cli app install dist/cofleet-test-0.1.0.lpk

# 使用本机已登录的懒猫客户端；创建临时测试 Agent，结束后自动清理
python3 scripts/verify-lazycat.py \
  --manager https://cofleet-test.YOUR-BOX.heiyu.space \
  --output /tmp/cofleet-verification.json
```

可添加 `--restart-container cloudlazycatappcofleettest-app-1` 验证重启恢复。该选项会重启指定测试容器，验证脚本当前通过本机已核实的 Node 26.8.2 调用 lzc-cli。请勿指定其他业务容器。

## 仍未验证

- Windows、macOS 电脑客户端的真实登录及后台运行。
- 跨账号真实用户隔离（当前通过模拟 Ingress 的自动测试覆盖，实机仅使用一个账号）。
- 登出、账号切换、长期运行和不登录桌面时的客户端会话行为。
- 外部网络/中继条件、设备服务网关、容器执行和应用部署等后续功能。

管理端节点表仍为内存状态，重启后由在线 Agent 重新上报；本轮成功不代表已实现任务执行、持久状态库或生产设备配对授权。
