# 阶段 4：多节点协作（0.6.0）

实现日期：2026-11-03。当前完成代码、Windows 原生编译与自动化测试；第二台实体节点和 Windows Docker Desktop 的完整发布闭环仍需实机验收。

## 已实现

- Agent 上报 `physical_host_id`、GPU、数据位置和长期在线偏好。Windows 与同机 WSL 使用同一个物理主机 ID，调度容量取同一物理主机各执行环境报告的最大值，不相加。
- 管理端保存轻量调度计划，并按物理主机扣减 CPU、内存承诺。候选逐台返回离线、暂停、执行器、资源、GPU 和数据位置不匹配原因。
- 批任务失联选择其他匹配节点并标记 `retry`；无状态服务标记 `rebuild`；有状态服务保留原放置并 `wait`，避免旧写入者状态未知时产生第二写入者。
- 管理页可创建、查看和释放调度计划；等待状态直接显示所有候选的拒绝原因。
- Windows Agent 支持 SCM 服务控制，具备停止通知与状态上报；Windows 上允许连接 Docker Desktop 的 Linux 容器引擎来启用阶段 1–3 执行器。

调度计划目前保存在管理进程内存中，表示放置意图和故障策略，不会替代阶段 3 的节点侧请求 ID 去重，也尚未自动提交具体构建。管理端重启后需重新创建计划。

## Windows Agent 构建和安装

在 Windows PowerShell 中：

```powershell
.\scripts\build-windows-agent.ps1
```

输出目录包含 `cofleet-agent.exe`、`cofleetctl.exe`、`cofleet-tray.exe` 和安装/卸载脚本。开发模式先在管理员 PowerShell 中设置测试凭据，再安装：

```powershell
$env:COFLEET_TOKEN = '<至少 16 字符的开发凭据>'
.\dist\windows-agent\install-agent.ps1 `
  -AgentExe .\dist\windows-agent\cofleet-agent.exe `
  -CtlExe .\dist\windows-agent\cofleetctl.exe `
  -Manager https://cofleet.example `
  -Name windows-node
```

要作为构建节点，先确认 Docker Desktop 使用 Linux containers，并传入本机批准的不可变工作空间镜像：

```powershell
.\dist\windows-agent\install-agent.ps1 ... -Docker `
  -WorkspaceImage 'sha256:<image-id>' -BuildDeploy
```

服务以 LocalSystem 启动，配置默认位于 `C:\ProgramData\CoFleet`。懒猫登录依赖桌面用户会话时，不应使用 LocalSystem 服务模式，需先做该 Windows 客户端版本的登录可用性实测。

需要读取当前桌面用户懒猫登录时，使用 `install-user-startup.ps1` 注册当前用户登录启动的 Agent 和托盘，不安装 LocalSystem 服务：

```powershell
.\install-user-startup.ps1 -AgentExe .\cofleet-agent.exe -TrayExe .\cofleet-tray.exe
```

配置调度能力（管理员 PowerShell）：

```powershell
$hostId = [guid]::NewGuid()
.\cofleetctl.exe --data-dir C:\ProgramData\CoFleet capabilities `
  --physical-host-id $hostId --gpu-name 'NVIDIA RTX' --gpu-memory-mib 12288 `
  --data-location home-projects --availability always-on
```

同机 WSL Agent 必须使用相同的 `$hostId`。`data-location` 是本机声明的稳定标签，不上传路径；有状态负载必须指定数据位置。

## 调度 API

`POST /api/v1/workloads` 创建或用同一 `workload_id` 幂等核对计划；请求需带 `X-CoFleet-Request: 1`。示例负载类型为 `batch`、`stateless_service`、`stateful_service`。`GET /api/v1/workloads` 会根据当前在线状态重新核对，`DELETE /api/v1/workloads/{id}` 释放资源承诺。

## 已自动验证

- Windows 与 WSL 两个执行环境不会把同一物理主机的内存容量计算两次。
- 资源承诺会阻止同一物理主机超配。
- GPU 或数据位置不匹配时进入等待，并返回具体原因。
- 有状态负载失联不迁移；批任务重试；无状态服务重建。
- Windows 目标上的 Agent、CLI、命名管道 ACL 和 SCM 入口可以编译，进程级 Agent/管理端闭环通过。

## 待实机验收

- 连接第二台实体 Windows 电脑，记录系统、Docker Desktop/WSL 版本和实际 `physical_host_id` 配置。
- 通过 SCM 重启、注销和系统重启验证自动恢复；检查 LocalSystem 与懒猫登录边界。
- 在 Docker Desktop Linux engine 上执行工作空间、构建、发布和回滚闭环。
- 断开原节点网络，分别观察批任务、无状态服务和有状态服务策略，并确认旧写入者隔离要求。
