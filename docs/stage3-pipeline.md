# 阶段 3：构建到部署（0.5.0）

管理页的「构建与发布」把持久开发空间接到节点上的原生 Docker。浏览器只提交操作并访问结果，构建与应用运行在指定电脑；微服负责认证、控制和应用流量代理。目前每节点一个应用入口，不包含跨节点调度、镜像仓库发布或数据库迁移执行器。

## 使用

在阶段 2 的 Agent 启动参数后增加 `--build-deploy`，保留 `--docker --workspace-image sha256:...`。此开关显式授权可信项目的 Docker 构建；默认不开启。本次验证 Docker 29.9.2 / Buildx，构建器须支持 `--resource` 限额参数；不支持时构建失败，不静默取消限额。

项目必须位于工作空间 `/home/coder/project` 下，包含 Dockerfile 和 `.cofleet-release.json`：

```json
{"data_policy":"stateless","migration":false}
```

应用监听 `0.0.0.0:8080`，`GET /health` 返回成功状态。示例见 [release fixture](../tests/fixtures/release/)。基础镜像应固定 digest，依赖应锁版本；构建 RUN 不提供网络，所需依赖须已在输入或基础镜像中。基础镜像和 Dockerfile frontend 的解析仍可能访问镜像仓库，不能视为完全离线构建。

1. 在开发空间编辑项目，管理页填写相对目录。
2. 点击「冻结快照并构建」，等待任务成功，读取构建日志。
3. 选择构建产物，点击「发布所选产物」。候选实例通过健康检查后切换入口。
4. 点击「访问应用」。后续版本可以再次构建发布，或「回滚上一版」。

稳定入口为 `/applications/<device-id>/`，需当前账号认证。开发模式通过管理页登录建立两个独立路径的会话 Cookie，分别用于工作空间与应用；平台模式使用现有懒猫登录。代理沿用来源、归属和当前 Agent 会话校验，只允许节点已批准的回环端口，不允许请求指定任意地址。平台凭据与浏览器 Cookie 不转发到应用，当前不支持应用自有 Cookie 登录或匿名分享；需支持入口子路径的应用。

本地命令同样支持 `cofleetctl --data-dir <node> pipeline status`、`logs <job-id>`、`build --request-id <uuid> --project-dir <relative>`、`deploy --request-id <uuid> --build-id <uuid> [--expected-current <release-id>]`、`rollback --request-id <uuid> --expected-current <release-id>` 和 `stop --request-id <uuid> [--expected-current <release-id>]`。首次发布预期版本为空，后续发布必须传当前版本。远程 API 沿用 `/api/v1/devices/<device-id>/docker`，请求为 `{"action":"pipeline","request":{...}}`。接受请求只表示已持久化排队，成功以任务状态为准。

## 输入、产物与恢复

冻结的是实际输入 tar 快照，不要求项目有 Git 仓库。快照 SHA-256、不可变 Docker image ID、任务状态和日志保存在节点私有目录。构建时再次校验快照哈希，后续编辑不影响已冻结输入；这不承诺位级可复现镜像。

快照拒绝越界路径、符号链接、硬链接及非普通文件；过滤 `.git`、依赖缓存、`.env*`、私钥常见扩展名等。过滤不是完整的秘密扫描，提交前仍需检查 Dockerfile 与源码。上限为 2048 文件、单文件 8 MiB、内容合计 24 MiB、归档 32 MiB。构建日志最多保留 2 MiB，API 返回尾部 4096 字节。

最多同时捕获两个输入，一个工作线程串行处理构建及发布。构建超时 300 秒，限制 1 GiB 内存和一个 CPU 配额。最多 32 份构建归档、128 条操作记录；达到上限拒绝新操作，不自动丢弃去重记录。当前没有自动清理、构建取消或历史分页，长期使用前需补充管理流程。页面只展示近期任务，完整记录仍在节点。

请求 ID 与输入参数一起持久化；同 ID 同参数重试返回原任务，不重复执行，参数冲突拒绝。发布、回滚、停止携带预期当前版本，过时操作不能覆盖新版本。重启后已冻结的排队任务继续，捕获中或执行中任务标记 interrupted，不静默重建。管理中心断开不停止现有应用。Agent 定期探测并恢复仍存在的当前容器；若容器被外部删除，明确报错要求重新发布。

## 健康与数据边界

候选实例使用确切 image ID，以非 root、只读根文件系统、无特权能力、256 MiB 内存、1 CPU、128 PIDs 运行，仅暴露回环动态端口。只有 `/tmp` 临时文件系统，不允许镜像声明的数据卷。健康探测最多 30 次，每次最多 2 秒、间隔 1 秒；失败保留旧版。成功后持久保存当前/上一版本再切换入口，停止旧实例并保留镜像。回滚重新验证旧镜像、启动候选并检查健康，不回退任务日志。

首版仅接受 `stateless` 且 `migration:false`，持久数据或迁移声明会被拒绝发布。声明不是自动代码审计；连接外部数据库的业务副作用仍由项目负责。镜像回滚不会恢复数据库、外部写入或改变后的数据结构。有状态发布必须先设计备份、兼容迁移和恢复方案。Docker 构建只适用于可信用户项目，不宣称敌对多租户隔离。

## 验证

原生集成命令（创建隔离测试节点、容器及卷）：

```sh
cargo build --locked --bins -j 1
python3 tests/pipeline_native.py --allow-native-docker --image sha256:YOUR_WORKSPACE_IMAGE_ID
```

11 项真实 Docker 场景覆盖冻结输入、去重、鉴权、不可变镜像、健康更新失败保留旧版、迁移拒绝、回滚、符号链接拒绝、失败日志、Agent 重启和显式停止防重放。浏览器还验证了 code-server 编辑 v1→v2、网页构建/发布、应用响应与回滚。结果见 [脱敏测试记录](stage3-pipeline-results.json)。

原生 omarchy 与真实微服链路的验证范围分别记录，不把本机访问微服域名当作独立外网验证。Windows/WSL2、macOS、独立外网设备及真实低性能客户端仍待测；当前实现了轻客户端闭环，不声称已完成这些硬件和网络验收。

真实微服已升级到 0.5.0，管理端二进制 SHA-256 与本次 LPK 内容一致。经平台登录完成冻结构建、日志读取与去重，访问 v1、健康更新 v2、保留上一版、回滚复用 v1 镜像并验证回滚重试；已有编辑器和 Node 项目服务仍健康。示例保留在工作空间 `cofleet-example/release-demo`，当前运行 v1，上一版为 v2；源码仍为 v2，可继续编辑构建。

复测脚本 `scripts/verify-pipeline-lazycat.py` 仅创建新示例，默认拒绝覆盖已有项目或应用；`--resume-first-release` 仅用于首次示例发布后代理状态尚未同步的中断恢复，并校验原始 fixture 文件未改变。首次发布成功到管理端下一次心跳就绪之间可能短暂返回 503，脚本等待就绪。原始证据与截图存放被忽略的 `.local/phase3/`。

本次目标 lzcos 1.6.2+fix.4、CLI 2.0.9；五篇官方打包规范在线校准成功，与阶段 2 摘要相同，未固定官方提交，来源及正文 SHA-256 见 [校准记录](deployment/pipeline-calibration.json)。沿用已验证的 `run_as: 1000`，CLI 仍产生旧的未知字段 lint 提示；未新增平台权限、Docker socket 或特权挂载。
