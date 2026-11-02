# 开发与运行原型

## 范围

第一版提供 Rust 管理端、后台 Agent、离线配置命令和可选托盘。支持 WebSocket 双向控制、心跳、节点展示、暂停/恢复接收新任务意图、断线退避重连和本地状态持久化。尚无任务执行器、容器管理、资源限制执行、生产配对系统或已验证的懒猫网关集成。

管理端默认使用共享测试凭据与内存节点表，重启后等待 Agent 重新上报。不能把这套测试身份机制用于多用户生产部署；实验懒猫模式已提供登录与用户隔离适配，并已在当前 Linux 客户端与默认微服验证真实登录。节点在线要求控制会话存在且未超过心跳周期的三倍（最少 15 秒）。

## 工具链与构建

安装 Rust stable、C 编译器；Linux 托盘还需要 GTK 3 和 Ayatana AppIndicator（或 libappindicator）。Linux 桌面需有支持 AppIndicator 的托盘宿主。

```sh
# 使用 rustup 且当前终端找不到 cargo 时，先执行：
# . "$HOME/.cargo/env"
cargo build --locked --bins
# 包含托盘；无桌面服务器不需要此 feature
cargo build --locked --bins --features tray
```

托盘仅使用系统托盘与原生菜单，不加载 WebView。Linux 使用 GTK/AppIndicator。管理页是原型静态页面，未引入前端打包链。

## 本机闭环

以下为 Bash 示例，在仓库根目录运行。不要把真实凭据提交到仓库或贴进日志。

```sh
# 生成一份随机测试凭据；保存于当前 shell 环境，不直接写入命令参数
export COFLEET_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
cargo run --bin cofleetctl -- --data-dir .cofleet-dev/node init \
  --name my-computer --manager http://127.0.0.1:9760
cargo run --bin cofleet-manager
```

另一个终端：

```sh
cargo run --bin cofleet-agent -- --data-dir .cofleet-dev/node
```

再开一个终端：

```sh
cargo run --features tray --bin cofleet-tray -- --data-dir .cofleet-dev/node
# 或使用无界面的控制命令
cargo run --bin cofleetctl -- --data-dir .cofleet-dev/node status
cargo run --bin cofleetctl -- --data-dir .cofleet-dev/node pause
cargo run --bin cofleetctl -- --data-dir .cofleet-dev/node resume
```

打开 [本机管理页面](http://127.0.0.1:9760/)，在页面内输入相同测试凭据。凭据只存于页面内存，不放入 URL 或浏览器持久存储。`pause` 仅保存并上报“不接收新任务”的意图；本版没有实际任务调度器。

退出托盘不会停止 Agent。终端运行的 Agent 通过 Ctrl+C 停止；Linux/macOS 也处理 SIGTERM。管理端关闭后，本地状态与暂停/恢复仍可用；恢复管理端后自动重连，不因重连更换设备 ID。

## 离线设置与本地权限

`cofleetctl config-path` 显示配置路径。默认使用当前用户的数据目录；所有进程必须使用同一个 `--data-dir`。该目录包含明文测试凭据，Unix 下要求当前用户所有、目录 0700、配置及 socket 0600。Windows 配置目录收紧继承权限，命名管道只允许当前用户与 SYSTEM，并拒绝远程客户端。

`agent.toml` 可用文本编辑器离线修改；连接地址、名称、凭据、心跳周期修改后重启 Agent。暂停/恢复会持久保存，不要求管理端在线；运行时发现其他配置已修改，会拒绝覆盖并提示先重启。不要在 Agent 运行时并行修改同一配置文件。

远程管理地址必须为 HTTPS；仅回环地址允许测试用 HTTP。管理端本身提供 HTTP，接入微服时需由已验证的平台 TLS 入口代理，并配置正确的 HTTPS 地址。Agent 不跳过证书检查，也不自动跟随重定向携带凭据。

Unix 使用独占文件锁防止重复 Agent，并在持有锁后清理崩溃残留 socket。单个 IPC 消息限制 16 KiB，读写超时 3 秒，并发最多 16 个。Windows 使用具备用户 ACL 的命名管道，第一实例标记用于防止重复启动。

## 后台服务

`deploy/linux/cofleet-agent.service` 是**无桌面、专用账号**的系统服务样例，未自动安装。需要先创建账号、安装二进制、初始化 `/var/lib/cofleet` 中的配置，再由管理员审查并安装。该账号的数据目录不能被桌面用户托盘访问；若需要托盘，Agent 和托盘应使用同一操作系统账号与数据目录，并相应修改服务样例。当前不提供跨账号提权控制。

`deploy/macos/dev.cofleet.agent.plist` 是 LaunchDaemon 模板，需替换用户和绝对路径；同样未安装或在 macOS 验证。Windows 当前可运行普通后台进程，但尚未实现 Windows SCM 原生服务入口或安装程序，不应直接用 `sc create` 注册这个普通控制台程序。

## 验证

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --features tray -- -D warnings
cargo test --locked
cargo build --locked --bins
python3 tests/smoke.py
```

进程级测试覆盖错误凭据拒绝、节点上报、重复启动、暂停状态持久化、管理端中断恢复、离线本地控制与凭据不出现在状态中；Unix 额外检查权限、超大消息及异常退出后 socket 恢复。CI 已配置三平台检查，但配置 CI 不等于三平台已运行通过。

性能测量需使用 `cargo build --release --features tray --bins`，分别统计 Agent、托盘与管理端；浏览器单独记录。10 分钟以上的空闲基线与三平台实际内存预算仍待测量，不以 debug 构建推断最终占用。

Linux 桌面有托盘宿主与 `gdbus` 时，还可执行 `python3 tests/tray_smoke.py`，通过真实托盘菜单验证暂停、恢复和退出。不要在无桌面环境运行这项检查。

Linux 空闲采样脚本：

```sh
python3 tests/measure_idle.py --seconds 600 --tray --output /tmp/cofleet-idle.json
```

脚本在临时目录初始化独立测试节点，采样后停止自己启动的进程并清理临时配置，不安装后台服务。RSS 包含共享页，PSS 按比例分摊共享页，私有页单独统计；单核 CPU 百分比由 `/proc` CPU 时间差计算。

## 参考 lzc-mesh 的长连接控制

控制面已由 HTTP 定时上报改为 Agent 主动建立的 `/api/ws` 长连接。配置文件默认兼容旧的开发模式配置，但旧版只支持 HTTP 心跳的 Agent 与新版管理端不兼容，需要同时更新二者。协议与身份边界见 [架构对应文档](lzc-mesh-reference.md)。

实验懒猫模式初始化（不会查询本机登录，Agent 启动连接时才查询）：

```sh
cargo run --bin cofleetctl -- --data-dir .cofleet-dev/lazycat init \
  --auth lazycat --manager https://cofleet.YOUR-BOX.heiyu.space --name my-computer
```

可添加 `--lzc-core-config /absolute/path/core.cfg` 覆盖本机懒猫客户端配置位置。此模式不需要 `COFLEET_TOKEN`，要求应用地址属于当前默认微服域名。Agent 需能访问登录用户的本机懒猫客户端配置及回环接口；专用系统账号不会自动继承桌面用户登录。无人登录时平台客户端是否仍可提供会话，必须另行实机验证。不要将实际 Cookie 或本机接口凭据填写到 CoFleet 配置。

管理端通过 `--auth lazycat --trusted-ingress-ip <实际可信代理IP>` 显式启用平台身份模式；这不是完整部署命令，必须先核实平台版本、可信入口及后端网络隔离。不得为方便而将任意客户端地址加入信任清单。仓库根目录已提供本次测试版的 LPK 配置，并完成真实部署；目标版本及网络边界见 [部署验证记录](deployment/lazycat-validation.md)。

开发模式可通过 `POST /api/v1/devices/{device_id}/refresh`（同一 Bearer 测试凭据）要求 Agent 立即上报，返回 202 与 `request_id`。节点列表中的 `last_report_id` 在 Agent 应答后更新；202 只代表已入队，不代表已完成。离线设备返回 409，其他账号的设备返回 404。本版不提供任意命令执行接口。

`cargo test --locked` 另外验证模拟 Ingress 的账号隔离、伪造 Header 拒绝、同设备重连替换、请求 ID 应答及非法协议/超大帧断开。模拟身份不等同真实懒猫认证测试。
