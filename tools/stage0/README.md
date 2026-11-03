# 阶段 0 设备通道探针

这是独立诊断工具，不属于 Rust Agent 的运行依赖，也没有给产品增加远程命令执行入口。

`local-probe.py` 在电脑的随机回环端口提供只读 HTTP 接口，将现有 Agent 的 IPC 状态作为响应；通过当前懒猫客户端注册临时服务。`app-probe` 必须在微服应用容器内执行，使用平台设备目录校验归属，再请求设备令牌并通过设备服务网关回调。每次使用随机服务名、接口密钥和响应挑战值。密钥通过子进程标准输入传递，不写入命令参数和报告。

前提：Linux 懒猫客户端已登录，Agent 已运行，`cofleetctl` 可用；应用具有设备目录访问能力和平台颁发的证书。运行时使用本机已安装的 `lzc-cli`，不自动部署应用或修改权限。

```sh
cd tools/stage0/app-probe
CGO_ENABLED=0 go build -o /tmp/cofleet-stage0-probe .
go test ./...
cd ../../..
lzc-cli project cp /tmp/cofleet-stage0-probe /tmp/
python3 tools/stage0/local-probe.py --container YOUR_APP_CONTAINER
```

`project cp` 需要在正确的项目和默认微服上下文执行，目标为目录。可通过 `--ctl`、`--data-dir` 指定现有节点，`--output` 指定脱敏结果路径，`--cases baseline` 仅复测正常回调。不要将探针直接放入公开 HTTP 路由。

## 鉴权边界

平台的本地服务注册字段会**注入**一个指定请求 Header；它不能证明调用者持有同名密钥。因此注册使用独立的网关标记，接口另外验证 `X-CoFleet-Probe-Key`。HTTP 回调不携带客户端证书，服务端 TLS 校验保持开启；取得设备令牌时使用平台应用证书。只有应用内平台 RPC 使用其注入的内部 h2c 地址。

探针覆盖正常回调、仅接口密钥、仅平台令牌、匿名、错误令牌、错误密钥和不存在的账号归属。未知账号测试不能替代两个真实账号之间的所有隔离测试。`Owner` 输入仅供容器内诊断；未来产品必须从可信认证上下文取得账号，不能公开接收任意 Owner。

本次 Linux 实测中，仅接口密钥也可以经网关访问测试服务。因此不能把平台令牌宣称为该 HTTP 服务的强制第二重鉴权；保密的接口密钥和设备归属校验仍然必需。

## 清理

脚本退出会关闭临时 HTTP 监听。当前客户端接口未发现注销服务方法，随机注册项会暂留，但后端已关闭；客户端内核重启后需重新注册。测试结束可在容器内删除 `/tmp/cofleet-stage0-probe`。不要为清理而重启整个微服或注销账号。

真实结果和限制见 [本机验证记录](../../docs/stage0-omarchy.md)。
