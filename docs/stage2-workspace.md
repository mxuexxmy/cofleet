# 阶段 2：持久开发空间

0.4.0 在原生 Linux Docker 上提供每节点一个持久开发空间，接入方式选择浏览器 code-server。项目、依赖、编辑器配置和扩展保存在同一节点的命名卷；稳定入口为 `/workspaces/<device-id>/`。Windows/macOS 与 WSL2 尚未验证。

本轮按当前条件使用 omarchy 经真实微服域名验证，另用隔离本地管理中心执行浏览器编辑与终端交互。没有独立外网设备，未切换手机蜂窝网络，也未强制使用公网中继。因此开发与当前路径验收完成，独立外网网络切换验收仍待测。

## 运行与持久化

开发镜像以官方 code-server 4.144.0 的固定 digest 为基础，增加 Node.js/npm、Python/venv、Git、curl 和证书。镜像在电脑节点构建；微服只运行 Rust 管理中心。节点启动时需显式批准一个本地不可变镜像 ID，管理请求不能指定任意镜像、宿主目录或 Docker 参数。

```sh
docker build -t cofleet-workspace:0.4.0 deploy/workspace
docker image inspect cofleet-workspace:0.4.0 --format '{{.Id}}'
# 将上一步 sha256:... 原样传入；沿用现有节点配置和设备 ID
./target/release/cofleet-agent --data-dir .local/runtime/node --docker --workspace-image sha256:...
```

开发容器使用 UID/GID 1000、2 CPU、2 GiB 内存、256 个 PID、移除 capabilities 和 no-new-privileges；不挂载 Docker socket 或宿主项目目录。编辑器仅发布在宿主 `127.0.0.1` 的动态端口，外部客户端通过管理中心入口访问。容器具有正常开发所需的出站网络。

命名卷 `cofleet-workspace-home-<device-id>` 挂载于 `/home/coder`，项目目录 `/home/coder/project`。停止、重启或重建开发容器保留卷；控制 API 不提供删除卷操作。编辑器启用远端工作台存储，配置与扩展随 home 卷保留。依赖应装在项目、虚拟环境或用户 home 中；容器系统层的手工修改不承诺在重建后保留。

期望状态和请求 ID 在节点私有目录的 `workspace-state.json` 中持久保存（0600、同步、原子替换）；请求去重、旧启动请求不能覆盖较新的停止状态。最多保留 4096 个请求，不丢弃旧记录。控制循环每 10 秒核对一次；管理中心断开或退出 Agent 不停止工作空间。宿主重启后的自动恢复需要 Agent 已实际启动，后台安装验证仍属于待办。

## 项目服务策略与健康状态

容器启动时，若项目中存在 `.cofleet/start.sh`，会在项目目录以开发用户执行该脚本，输出写到 `/home/coder/.local/share/cofleet/project.log`。长期项目服务使用该脚本恢复；浏览器关闭不停止容器。容器内单个项目服务退出后不会被独立无限重启，需在终端修复、重启服务或重新启动工作空间。普通交互终端会话不保证在所有浏览器或网络切换后原样恢复，可新建终端继续访问同一项目。

Docker healthcheck 检查 code-server `/healthz`；Agent 另外检查工作空间内端口 3000 的 `/health`，管理页分别显示编辑器健康与项目服务健康。端口 3000 的服务可通过工作空间入口下的 `proxy/3000/` 预览。健康数据有采样延迟，编辑器健康不等同于所有扩展已加载或所有项目服务正常。

## 数据路径与授权

```text
浏览器 → 微服的已认证工作空间路由
       → 独立反向 WSS 数据连接
       → omarchy Agent → 本机回环端口 → 开发容器
```

Agent 的原 WSS 保持心跳、状态与有限操作；每条 HTTP/升级连接使用独立数据通道，按账号、设备、当前会话和一次性隧道 ID 校验。每节点最多 32 条、管理中心总共最多 64 条数据连接；16 KiB 分块、32 KiB 有界管道及背压，上传上限 32 MiB。不会把项目内容塞进控制消息，也没有接受任意目标地址的转发 API。

HTTP 与 WebSocket 都先验证管理端账号与浏览器 Origin。平台登录 Cookie、Authorization、身份 Header 不转发给开发容器；开发应用返回的 Set-Cookie 也不写回管理中心域名。TLS 校验始终开启。当前项目预览未实现独立 Cookie 登录流程，不能把它当作通用应用发布入口。内部 code-server 使用外部认证模式；其回环入口不能作为公开服务暴露。

Node Inspector 的固定 `proxy/9229/` 内部代理会把 Host/Origin 规范为本地地址，以兼容其 DNS 重绑定保护。公网请求的账号和来源检查在改写前完成，并未关闭外层认证或 TLS。

开发模式额外提供仅对工作空间路径有效的 HttpOnly、SameSite=Strict 浏览器会话 Cookie，用于从管理页进入编辑器；其值与开发 Bearer token 不同，退出管理页会清除该 Cookie。懒猫模式沿用平台登录，不新增公开绕过入口。

工作空间仅在本机显式批准镜像后启用。开发用户可以在该容器和卷内编辑并执行任意项目代码；这是开发空间功能，不是对宿主 Docker 的任意命令接口。当前仍使用原型的账号/节点会话模型，尚无完整设备配对或共享工作空间授权系统。

## 管理与复现

管理页支持启动、停止、进入开发空间、项目预览和日志。CLI：

```sh
cofleetctl --data-dir .local/runtime/node workspace list
cofleetctl --data-dir .local/runtime/node workspace start --request-id <uuid>
cofleetctl --data-dir .local/runtime/node workspace logs
cofleetctl --data-dir .local/runtime/node workspace stop --request-id <another-uuid>
```

API 沿用 `POST /api/v1/devices/<id>/docker`，请求形如 `{"action":"workspace","request":{"action":"set_running","request_id":"<uuid>","running":true}}`；子动作包括 `status`、`set_running`、`logs`。HTTP 200 仍需检查 `error`。超时请复用原请求 ID，不能当作“没有执行”。

原生 Docker 集成测试会创建和清理自己的临时节点、容器、卷，且不会重启宿主 Docker daemon：

```sh
python3 tests/workspace_native.py --allow-native-docker --bin-dir target/release --image sha256:...
```

真实微服验证使用现有已启用工作空间的节点：

```sh
python3 scripts/verify-workspace-lazycat.py --manager https://YOUR_APP_DOMAIN --data-dir YOUR_NODE_DIR --output .local/phase2/micro-workspace.json --seed-example
```

`--seed-example` 只允许创建尚不存在的 `cofleet-example` 和项目启动脚本，遇到已有文件拒绝覆盖；之后复测不带该参数。脚本会在这个示例中执行读写、shell 和调试验证，并保持工作空间运行。登录 Cookie 只在内存中，不写入报告。

## 实测证据与边界

日期为 2026-11-03，omarchy 原生 Linux x86_64、Docker 29.9.2；微服 LC 02、lzcos 1.6.2+fix.4、CLI 2.0.9。原始凭据、私有设备信息和截图保留在忽略提交的 `.local/phase2/`；脱敏结果见 [stage2-workspace-results.json](stage2-workspace-results.json)。

原生集成验证通过：创建和健康状态、请求去重、编辑器 HTTP 反向通道、匿名与跨 Origin 拒绝、项目与本地依赖、凭据不泄漏、真实断点/变量检查/继续执行、管理中心重启后重入、删除并重建开发容器后保留代码和已装依赖、按项目脚本恢复服务、显式停止保留卷及私有状态文件。

浏览器实际完成：打开编辑器、修改并保存文件、终端安装离线测试依赖并调用；关闭页面后，文件、依赖和项目服务仍在；重新进入同一入口并打开新终端。未配置的 GitHub 身份扩展曾报告超时，不把它记为“控制台完全无错误”；本轮基础编辑、终端和调试不依赖 GitHub 登录。

真实微服域名验证覆盖编辑器/项目代理、请求去重、平台凭据隔离、实际断点及变量调试、容器内远程文件写入和 shell 读取、新连接再次读取相同项目。浏览器交互是在隔离本地入口测试，微服端使用实际认证 API 与调试 WebSocket，二者记录分开。

LC 02 资源和网络观测随同 JSON 报告保存。该路径可能由懒猫选择局域网直连，不能作为独立公网性能结论；Docker stats 是当时快照，不是严格受控的峰值实验。未执行宿主/Docker daemon 重启、真实异网切换、三平台兼容或性能承诺。

## 打包与参考

微服只更新应用版本/描述，不添加 Docker socket、特权或权限。五篇官方打包规范重新校准，摘要与前次一致，来源和 SHA-256 见 [workspace-calibration.json](deployment/workspace-calibration.json)。沿用已验证的系统 `run_as` 能力，CLI 的旧 lint 仍提示字段未识别。

code-server 接入与子路径配置参照官方 [Usage](https://coder.com/docs/code-server/guide)、[FAQ](https://coder.com/docs/code-server/FAQ)；基线版本来自官方 [4.144.0 发布记录](https://github.com/coder/code-server/releases/tag/v4.144.0)。

### LC 02 当前路径观测

| 指标 | 实测值 |
| --- | --- |
| 无下载时管理容器 | CPU 0.01%，内存 14.27 MiB |
| 传输期间一次快照 | CPU 2.48%，内存 13.95 MiB |
| 10 次项目健康请求 | 中位 25.21 ms，最大 32.72 ms |
| 连续 8 次、每次 4 MiB 下载 | 共 32 MiB / 1.967 s，约 16.27 MiB/s |

这是管理应用容器的整体观测值，包含其运行环境，不是代理逻辑的独立增量；不包含 omarchy 上 code-server 的内存。采样不能证明峰值开销，当前路由也不代表异地公网。

最终实际节点的 Agent 更新后重新连接，同一个开发容器和项目服务仍健康。该开发空间保持运行，提供 `cofleet-example` 示例；已有受管测试容器的运行意图保持不变。

本轮临时 UI 验证的管理进程和容器会清理，测试卷保留在本机供复查；实际微服对应的开发空间继续运行。

补充限制：多标签 UI 验证后的清理阶段，内置浏览器报告了页面渲染崩溃，原因未确定，未将其归因为服务端。实际微服和节点健康检查仍正常；外部浏览器兼容性与长时间浏览器会话需继续补测。
