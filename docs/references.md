# 参考项目与研究依据

研究日期：2026-11-02。以下实现核查基于当时读取的提交；后续开发需要重新验证目标平台版本与 API 可用性。

## 懒猫设备接力（lzc-mesh）

- 仓库：https://github.com/ponzS/lzc-mesh
- 已读提交：`cc9374d0ff26122e7da9d41f7b75f6d6e5b3f40f`
- 客户端认证发现：`lzc-mesh-client/windows/src/lzc.cpp`
- WSS 连接：`lzc-mesh-client/windows/src/net.cpp`
- 中枢与消息协议：`lzc-mesh-lpk/hub/src/main.rs`、`protocol.rs`
- 直连实现：`lzc-mesh-client/transport/src/`

借鉴点：复用懒猫登录和网络、用户隔离的中枢通信、节点连接管理。已按此方向实现 CoFleet 的 WebSocket 控制原型和实验登录适配，详见 [架构对应](lzc-mesh-reference.md)。仓库说明采用 PolyForm Noncommercial 许可；复用代码前须单独检查许可证，不能把它当作宽松开源许可。

## 远程电脑（lightos-remote-desktop）

- 仓库：https://github.com/ca-x/lightos-remote-desktop
- 已读提交：`0ed70a91cdf4ea95dd02a1f7cf4ba1b979e676c3`
- 设备服务注册：`src/agent/network.rs`、`src/agent/network/wire.rs`
- 设备归属与桌面代理：`manager/hosts.go`
- 远程管理调用：`manager/control.go`
- 设计说明：`docs/management-lpk.md`

借鉴点：LocalServiceGateway、设备入口与访问令牌、注册续期、本机控制服务认证。项目存在不同部署与兼容路径，不能混为单一通用接口；原生 Windows/macOS 桌面能力及目标客户端兼容性需实际验证。

## 懒猫开发文档

- HTTP 路由：https://developer.lazycat.cloud/advanced-route.html
- TCP/UDP 四层转发：https://developer.lazycat.cloud/advanced-l4forward.html
- LightOS 场景：https://developer.lazycat.cloud/advanced-lightos.html
- OIDC：https://developer.lazycat.cloud/advanced-oidc.html
- 自建中继：https://developer.lazycat.cloud/network-pass-through.html

可复用已集成至懒猫网络的中继，不把另建中继列为项目前置工作。官方文档并不直接证明所有外部电脑或 WSL2 服务均已可达。

## Kubernetes 管理经验

- 架构：https://kubernetes.io/docs/concepts/architecture/
- 控制循环：https://kubernetes.io/docs/concepts/architecture/controller/
- 节点：https://kubernetes.io/docs/concepts/architecture/nodes/
- 资源管理：https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/
- 节点选择：https://kubernetes.io/docs/concepts/scheduling-eviction/assign-pod-node/
- 持久卷：https://kubernetes.io/docs/concepts/storage/persistent-volumes/

借鉴设计原则不意味着项目已采用 Kubernetes，也不承诺兼容其 API。

## 尚未核实的来源

- 用户提供的 X 帖子：https://x.com/yifanxu_ephai/status/2117115141162426700
- 读取时返回 403，未将帖子内容作为事实依据。
