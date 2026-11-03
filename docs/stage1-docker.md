# 阶段 1：原生 Docker 指定节点管理

2026-11-03，在 omarchy（原生 Linux x86_64、Omarchy 4.5.0rc3、Docker Engine 29.9.2 / API 1.56）进行验证。WSL2 后续单独测试。

## 已实现

管理页显示节点 Docker 容器的名称、镜像和实际状态；对当前节点的固定测试容器提供启动、停止、最近日志和刷新操作。其他容器只读展示。Agent 使用 `--docker` 显式启用，默认不开启；连接原生本机 `/var/run/docker.sock`，不使用远端 Docker context。

测试容器按设备 ID 使用确定的名称，携带设备归属和规格标签。每次修改先检查标签、镜像和归属，再用不可变容器 ID 操作；同名非受管容器会被拒绝。测试镜像固定为 Alpine 3.22 的 digest，使用本地已有镜像，不在控制请求中自动拉取：

```
alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8
```

容器使用非 root 用户、只读根文件系统、无网络、无宿主目录挂载、不开放端口、移除 capabilities，并设置 no-new-privileges、32 MiB 内存、0.25 CPU、16 个 PID 和有上限的本地日志。容器内命令是固定的启动消息和心跳，不接受外部命令、镜像、挂载或环境变量。

## 持久化和重试语义

节点的私有数据目录保存 `docker-state.json`（0600），包含设备 ID、期望运行状态、变更代次和已接受请求 ID。通过同目录临时文件、文件同步、原子替换及目录同步，在执行 Docker 操作前保存意图。写入失败不开始新操作；损坏文件使 Agent 拒绝静默重置状态。

- 同一请求 ID、相同内容：返回 `duplicate: true`，核对**最新**期望状态。
- 同一请求 ID、不同内容：返回错误，不改变状态。
- 新请求：先保存意图，再执行；失败或超时后使用相同 ID 重试。
- 已显式停止后再重试更早的启动请求：保持停止。
- 每 10 秒核对一次已保存的状态；Agent 重启后继续核对。Docker restart policy 为 `no`，由一个控制循环负责恢复。
- 管理中心连接变化和 Agent 退出不会停止容器。暂停接收新任务会拒绝新启动意图，仍允许显式停止与既有意图恢复。

请求日志最多 4096 项，达到上限拒绝新意图，不淘汰旧记录以免迟到的请求被重新执行。当前管理端设备目录仍在内存中，重启后由 Agent 重新上报；容器期望状态由节点保存。

## API 和本地操作

Web 操作经现有 WSS 通道发送给已认证账号的在线节点。每个请求携带独立传输 ID；响应必须来自相同账号、设备和当前会话。HTTP 命令接口要求 JSON 和 `X-CoFleet-Request: 1`，无跨域 CORS 授权。超时返回 504，表示结果未知，调用者必须复用原操作请求 ID。

`POST /api/v1/devices/<device-id>/docker`：

| JSON 内容 | 行为 |
| --- | --- |
| `{"action":"list"}` | 读取容器快照 |
| `{"action":"set_test","request_id":"<uuid>","running":true}` | 保存运行意图并核对 |
| `{"action":"set_test","request_id":"<uuid>","running":false}` | 保存停止意图并核对 |
| `{"action":"test_logs"}` | 读取测试容器最近 40 行，上限 4096 字节 |

业务失败通过响应的 `error` 字段报告，并尽可能返回当前快照；不能只凭 HTTP 200 判断启停成功。容器列表最多 32 项，受管测试容器优先，另有总消息大小限制和截断标志。

在现有节点配置上运行：

```sh
cargo build --locked --release --features tray --bins
./target/release/cofleet-agent --data-dir .local/runtime/node --docker
./target/release/cofleetctl --data-dir .local/runtime/node docker list
./target/release/cofleetctl --data-dir .local/runtime/node docker start --request-id <uuid>
./target/release/cofleetctl --data-dir .local/runtime/node docker logs
./target/release/cofleetctl --data-dir .local/runtime/node docker stop --request-id <another-uuid>
```

需要现有用户已经能访问本机 Docker；程序不调整用户组、socket 权限或 Docker daemon 配置。测试镜像须预先准备。资源预留仍是规划信息，测试容器的固定限制独立于预留额度。

## 实机验收

复现：`python3 tests/docker_native.py --allow-native-docker --bin-dir target/debug`。脚本建立独立开发管理中心和节点，测试后停止其进程，仅清理由该次测试创建并记录的容器 ID，不重启 Docker daemon。脱敏结果见 [stage1-docker-results.json](stage1-docker-results.json)。

通过的场景：

1. 请求认证、浏览器请求保护和容器列表。
2. 同名非受管容器拒绝操作；删除本次测试构造的冲突后恢复执行。
3. 调用者在 1 毫秒等待超时后放弃响应，复用请求 ID 重试；并发重试后仍只有一个实例。
4. 日志读取及容器的真实资源和隔离配置。
5. 管理中心断开时容器继续运行；管理中心重启后节点重新上报。
6. Agent 退出后将测试容器停止，再启动同一 Agent，恢复持久化运行意图，复用原容器 ID。
7. 显式停止后重启 Agent，保持停止；迟到的旧启动请求与冲突请求均不能覆盖停止状态。
8. 本机暂停时拒绝新启动；期望状态文件权限和内容持久化。

本轮范围为固定测试容器及其控制闭环。通用容器编排、任意任务、删除入口、镜像分发、自动调度、Windows/macOS 和 WSL2 不在本轮验收范围。宿主机重启与 Docker daemon 重启未在本次执行，以免影响现有应用。

## 微服打包校准

目标沿用 LC 02 的 lzcos 1.6.2+fix.4、lzc-cli 2.0.9。已重新取得 package、manifest、build、deploy-params、LPK format 五篇官方原文，与先前相同主题摘要无变化；每篇来源和 SHA-256 见 [docker-calibration.json](deployment/docker-calibration.json)。只更新应用版本和描述，未给微服添加 Docker socket、特权或额外权限。现有 `run_as` 的 CLI lint 提示不识别字段，仍按已验证的 1.6.0+ 系统能力使用。

## 交付运行状态

微服测试应用和本机 Agent、托盘均更新至 0.3.0。本机 Agent 使用原设备 ID、原配置并启用 `--docker`。真实懒猫登录下，微服通过 WSS 成功读取本机容器列表、启动测试容器、读取日志、去重重试和显式停止；迟到的旧启动重试保持停止。

网页交互另用同一 release 的隔离本地管理中心验证：点击启动、读取日志和停止均成功，页面显示实际运行/退出状态，控制台无错误。该临时管理中心、Agent 和临时容器在验证后清理。用户 Chrome 的连接中途不可用，内置浏览器没有微服登录态，因此没有把本地页面交互冒充真实微服页面点击验证；真实微服链路由认证 API 实测。

最终原 Agent 与托盘保持运行，连接微服成功。正式验证节点的受管测试容器保持停止，保留日志供检查；已有其他容器未被修改。

实现所用容器参数和重启策略参照 Docker 官方 [container run](https://docs.docker.com/reference/cli/docker/container/run/) 与 [restart policy](https://docs.docker.com/engine/containers/start-containers-automatically/) 文档。
