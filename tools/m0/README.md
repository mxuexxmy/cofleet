# M0：独立入口与应用登录验证工具

这是实验工具，不是通用 Docker 网关。只有显式启用 `access-probe` feature 才编译；正常 manager／agent 构建和当前微服应用配置不变。

本轮结果及阻塞项见 [实测记录](../../docs/m0-access.md)。**前缀域名下的应用 JavaScript 可以写父域 Cookie，正式访问门槛未通过。** 浏览器脚本默认用于收集结果，退出成功只表示实验执行结束；不能据此认定安全门槛通过。设置 `M0_REQUIRE_STRICT_ISOLATION=1` 会在该门槛未通过时返回非零状态。

## 组成

- `probe.rs`：独立微服网关与本机连接器。仅允许已配置的一个用户和三类固定测试目标；使用现有 CoFleet 的 `agent_tunnel`／`serve_tunnel` 数据流，不注册或修改生产节点。
- `proxy.rs`：实验性的 Cookie、身份 Header、重定向与 HTTPS 校验策略。尚未接入生产代理；不能视作完整通用代理实现。
- `fixture.py`：标准库测试 Web 应用、静态服务及自签名 HTTPS 服务；自身 Cookie 登录和 WebSocket 回显。通过原生 Docker CLI 独立启动，不归 CoFleet 部署控制循环管理。
- `run.py`：创建、打包、安装、协议验证和清理。只操作带本次标签的临时容器及 `xyz.mxue.lzc.app.cofleet.m0.*` 临时包。
- `browser.mjs`：使用一次性 Chromium context 验证真实表单登录、Cookie、WebSocket、来源隔离及父域 Cookie 边界；不导出浏览器会话。

## 前提

需要已登录的懒猫桌面客户端、可用的默认微服、现有 CoFleet 管理端和节点配置、Docker、OpenSSL、Rust musl target、Python venv，以及 Playwright／Chromium。脚本默认沿用本机 `mise exec node@26.8.2 -- lzc-cli`；其他环境需先校对 CLI 调用。

管理端地址从被忽略的 `.local/current-manager-url` 读取；节点配置从 `.local/runtime/node` 读取，仅用于构造独立的实验连接器，不改写原文件。平台 Cookie 只在内存中使用。运行前需先按懒猫开发技能校准真实系统／CLI 版本和路由、多域名、身份 Header 规范。

默认复用本机已有的 `cofleet-workspace:0.4.0` 镜像运行 Python 标准库夹具，也可通过 `M0_FIXTURE_IMAGE` 选择其他已在本地且包含 `python3` 的批准镜像；脚本不拉取镜像。测试容器使用当前 UID/GID、只读根文件系统、只读夹具挂载、移除 capabilities、不提升权限，并限制 CPU／内存／进程数；三个端口只发布到 `127.0.0.1`。

## 构建与执行

```sh
cargo test --locked --features access-probe --bin cofleet-access-probe
cargo build --locked --release --target x86_64-unknown-linux-musl \
  --features access-probe --bin cofleet-access-probe
python3 -m venv .local/m0/venv
.local/m0/venv/bin/pip install -r tools/m0/requirements.txt
python3 tools/m0/run.py prepare
python3 tools/m0/run.py deploy
.local/m0/venv/bin/python tools/m0/run.py verify
node tools/m0/browser.mjs
```

若本机 C 依赖需要 musl wrapper，设置 `CC_x86_64_unknown_linux_musl`；Rust 链接器保持默认，不能将仅供 C 编译的 musl specs wrapper 同时作为 Rust 链接器。Playwright 模块可以通过 `NODE_PATH` 定位，Chromium 可通过 `CHROMIUM_PATH` 指定。

临时包默认使用随机、不含连字符的基础子域名，前缀为 `login-`、`static-` 和 `badtls-`；实际分配域名必须与 `LAZYCAT_APP_DOMAIN` 一致，网关不一致时拒绝启动。M0 仅对白名单内的本机固定服务建立隧道，没有地址／端口输入 API。

实验网关将夹具自身的 `http://127.0.0.1:8800` 视为唯一批准的登录服务绝对重定向来源；这是夹具容器内固定端口，不是随机发布的宿主端口。任意不匹配的回环绝对重定向返回 502，不猜测或探测其他本机服务。

所有配置、一次性 fixture 密码、实验连接密钥、自签名私钥、临时 LPK、运行日志、截图和设备快照保存在 `.local/m0/`，不提交仓库。临时包中的连接密钥只用于该次实验，包卸载后失效；它不是平台 Cookie。不要发布原始 LPK 或整目录。

## 必须清理

无论验证是否成功，都执行：

```sh
python3 tools/m0/run.py cleanup
```

清理会移除本轮临时微服包和 Docker 测试容器，并核对原在线设备、预留／能力配置、开发空间、已发布应用的标识和调度计划。必须先清理当前实验，才能再次 prepare。保留忽略目录下的结果用于复盘；公开记录只提取布尔结果及脱敏信息。

M0 不修改现有管理端、不增加容器发现或持久入口功能，也没有替换已有应用的管理工具。第二真实账号验证和平台端会话撤销另行安排，不能用伪造 Header 用例或浏览器清 Cookie 冒充。
