# 参考 lzc-mesh 的控制架构

核查日期：2026-11-02。参考仓库 [ponzS/lzc-mesh](https://github.com/ponzS/lzc-mesh)，固定提交 `cc9374d0ff26122e7da9d41f7b75f6d6e5b3f40f`。

## 采用的分工

```text
管理网页 ── 同源 HTTP / 平台登录 ── 微服控制中枢
                                      ↑
                            Agent 主动发起 WSS 长连接
                                      │
                   本地 IPC ── 电脑后台 Agent ── 执行环境（后续）
                      │
                   轻量托盘
```

参考项目将 Rust 中枢放在微服 LPK 内，客户端主动连接 `/api/ws`；中枢从可信 Ingress 的 `X-HC-User-ID` 确定用户，在用户自己的设备组中管理会话。客户端从本机懒猫客户端取得登录会话，而不是要求用户手填另一个产品的账号密码。

CoFleet 采用上述控制通道与身份分工，仍保留 Rust 跨平台 Agent 和独立托盘。lzc-mesh 各平台分别使用 Swift、C++、C；参考其通信架构不要求采用相同客户端语言。

## 原型实现对应

| lzc-mesh 机制 | CoFleet 实现 |
| --- | --- |
| 客户端主动连接 `/api/ws` | Agent 使用 WS/WSS 连接同一路径；远程必须 WSS |
| 首帧 Hello | `hello` 携带协议版本与节点状态；5 秒内必须完成 |
| 按用户分组 | Hub 以 `(可信用户身份, device_id)` 索引节点 |
| 中枢双向消息 | 心跳、确认、中枢请求状态、Agent 带请求 ID 回复 |
| 客户端重连 | 带随机抖动的退避；原设备 ID 保留 |
| 会话独立于业务状态 | 连接替换使用 session_id，旧连接清理不会删除新会话 |
| 本机懒猫登录发现 | 独立 `lazycat` 模块，重连时重新获取，连接期间每 60 秒检查登录变化 |

代码：`src/hub.rs`、`src/auth.rs`、`src/lazycat.rs`、`src/protocol.rs` 和两个服务端/Agent 入口。实现为 CoFleet 自行编写的协议与业务代码，不与 lzc-mesh 线协议兼容。

消息使用带 `type` 的 JSON：

```text
Agent → Hub: hello { protocol_version: 1, state }
Hub → Agent: welcome { protocol_version: 1, session_id }
Agent → Hub: heartbeat { state }
Hub → Agent: ack { last_seen_unix }
Hub → Agent: request_report { request_id }
Agent → Hub: report { request_id, state }
```

主动请求状态用于验证下行控制，不执行远程命令、不操作容器。未来启用任务执行前仍需建立独立设备授权与配对机制，当前账号会话不代替完整设备身份认证。设备 ID 在单个会话中不可变；报告必须对应中枢发出的待处理请求 ID。暂停/恢复仍由本机保存意图并通过该长连接立即上报。

## 身份与信任边界

管理端有两个显式互斥模式：

- `development`：共享测试凭据，所有节点属于独立的开发设备组；忽略客户端自行携带的用户身份 Header。
- `lazycat`：不接受共享测试凭据代替平台登录，只接受直接网络对端属于 `--trusted-ingress-ip` 清单、且带非空 `X-HC-User-ID` 的请求。不会使用 `X-Forwarded-For` 来放宽代理信任。

指定代理 IP 不能替代网络隔离。正式部署必须保证只有平台可信入口能够访问后端，入口清除客户端自报身份并注入真实身份；不可将普通客户端地址加入信任清单。测试中的回环信任仅模拟 Ingress，不构成真实平台身份认证。Agent 专用 WebSocket 及状态请求 API 拒绝带 Origin 的浏览器调用，管理网页仍通过同源只读 API 获取节点。

电脑端的 `lazycat` 登录适配读取本机 `~/.config/lzc-client-desktop/core.cfg`（也可指定路径），使用回环端口及 `LZC_MISC_AUTH_TOKEN` 请求 `/default-box`，随后通过 `/box-ext-info` 获取 `Lzc-Auth-Token` Cookie。只向默认微服域名下、443 端口的 HTTPS 应用子域发送会话，禁止跨域或明文发送；不支持手动切换到未登录的其他微服。

本地 Cookie 和客户端接口凭据不写入 CoFleet 配置、状态或日志。连接中每 60 秒重新检查本机登录；凭据变化或本机登录不可用时断开重连，因此不能承诺注销或切换账号的瞬时撤销。实际客户端接口兼容性、账号切换行为与微服入口仍需实机核验。

## 资源与连接约束

- WebSocket 帧/消息最多 16 KiB；Hello 及发送操作超时 5 秒。
- 管理端最多 256 条控制连接，包含尚未完成 Hello 的连接；每账号最多保留 256 个节点，全局最多 4096 个。
- 每连接下行队列及未完成状态请求最多 8 条，积压时明确拒绝新请求。
- 超过三倍心跳周期未收到有效状态则断开，最短 15 秒；控制帧不代替业务心跳。
- 节点快照仍为内存存储；离线记录在新连接注册时清理 24 小时以上的旧记录。管理端重启后由 Agent 重新注册。

## 本轮未引入

lzc-mesh 的 QUIC、蓝牙、键鼠事件、剪贴板及文件共享服务均不属于当前任务。控制通道不承载将来的大文件与镜像数据流；工作空间和应用访问仍需另外验证设备网关或平台路由。

已针对默认微服 `lzcos 1.6.2+fix.4 / lzc-cli 2.0.9` 完成相关官方规范校准，生成并部署独立测试版；真实 Linux 登录和 WSS 控制验证通过。详情及其余未验证项见 [部署记录](deployment/lazycat-validation.md)。

## 参考依据

- [中枢入口与身份提取](https://github.com/ponzS/lzc-mesh/blob/cc9374d0ff26122e7da9d41f7b75f6d6e5b3f40f/lzc-mesh-lpk/hub/src/main.rs)
- [参考消息协议](https://github.com/ponzS/lzc-mesh/blob/cc9374d0ff26122e7da9d41f7b75f6d6e5b3f40f/lzc-mesh-lpk/hub/src/protocol.rs)
- [Windows 客户端登录发现](https://github.com/ponzS/lzc-mesh/blob/cc9374d0ff26122e7da9d41f7b75f6d6e5b3f40f/lzc-mesh-client/windows/src/lzc.cpp)
- [官方 HTTP 身份 Header 说明](https://developer.lazycat.cloud/http-request-headers.html)

官方身份 Header 页面已取得有效 VitePress 正文，HTML SHA-256 为 `4dedbbe2e1966bd35a266ea2a1188a3093df00f82768302f7f5827590b717813`；其中说明的 Ingress 用户 Header 与本方案一致。该页面描述浏览器入口的 `HC-Auth-Token`，参考客户端源码使用内部客户端导出的 `Lzc-Auth-Token`；不能据此将两者当作通用可互换 Cookie。本轮只按参考客户端接口实现实验适配，后续已完成上述目标版本校准及单一 Linux 客户端的真实登录验证，其他客户端版本仍需实测。

参考仓库采用 PolyForm Noncommercial 许可。这里只借鉴架构与接口分工，未引入其源文件或业务实现；后续如需复用其代码，应单独核对许可及使用场景。
