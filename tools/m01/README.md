# M0.1：自动前缀域名的认证隔离实验

本目录包含隔离实验与真实部署验收。实验夹具不接入生产；0.7.1 Manager 另已实现用户明确选择的受信任应用模式，并使用真实部署脚本验收。

## 本机验证

```sh
NODE_PATH=<包含 playwright 的 node_modules> node tools/m01/browser.mjs
```

需要 Python 3、OpenSSL、Chromium、Playwright。脚本生成一次性自签名证书，启动仅监听 `127.0.0.1` 的 HTTPS 夹具；浏览器的主机解析规则把 `manager.cofleet.test`、`one.cofleet.test`、`two.cofleet.test` 映射到本机。不会修改 hosts、系统证书、DNS、平台配置或真实账号。

管理和应用主机共享 `cofleet.test`，故意复现平台前缀域的同 Site 条件。仅此临时浏览器 context 忽略自签名证书错误；这不构成生产 TLS 验收。两个虚构账号 alice/bob 仅用于夹具，不是 OIDC／微服认证实现。服务无生产密码、无 Docker socket，不转发真实应用。

夹具实现：严格 Host 路由、管理来源校验、`__Host-` HttpOnly 会话、应用端 bootstrap Cookie、绑定 bootstrap／Host／管理会话／revision 的单次票据、短期访问会话和撤销。票据通过 POST 表单兑换，不放入 URL。随机值服务端按 SHA-256 索引；各内存表按过期时间清理。浏览器测试退出后终止夹具进程。

结果与证书保存在 `.local/m01/probe-*/`，不提交私钥。实验没有覆盖长期负载、存储容量、时钟故障、票据过期边界或 WebSocket 撤销，不应复制夹具作为产品服务器。

## 真实前缀域验证

```sh
NODE_PATH=<包含 playwright 的 node_modules> node tools/m01/platform.mjs
```

要求原 M0 演示仍运行、懒猫客户端已连接并登录。脚本从本机客户端在内存中取得平台凭据，使用隔离浏览器访问原 M0 的基础域与 static 前缀域，正常验证平台 TLS。仅创建合成的 `__Host-cofleet_m01_probe` Cookie 和无害父域 marker，不修改真实平台凭据或应用登录状态，不导出令牌，不改变服务配置。浏览器 context 随测试关闭。

这里通过 `addCookies` 注入的是合成会话，用来验证真实域名上的浏览器 Cookie 规则，不是生产服务器签发会话或完整单次授权链路的验证。

## 结果

见 [M0.1 方案与结果](../../docs/m01-access.md)。17 项本机协议／浏览器检查和 4 项真实域名 Cookie 检查通过；共享父域普通 Cookie 写入依旧成立，完整 Cookie 隔离未通过。完整隔离模式不据此放行；受信任应用模式采用用户明确选择的不同信任边界。


## 真实受信任应用访问

先按根构建配置刷新 `.local/lpk-content`，再用 `lzc-cli project build deploy/lazycat-access` 构建独立验证包，安装后将专门的 Agent 以 `--docker-observe` 连接到实际分配域名。不要修改旧 Agent 配置或假设两个工作台共享状态。

```sh
COFLEET_M01_MANAGER_URL=https://<实际分配域名> NODE_PATH=<playwright模块目录> node tools/m01/live.mjs
```

`live.mjs` 会创建／复用 HTTP 验证与已有 M0 登录夹具入口，执行平台 TLS、浏览器授权、应用 Cookie 登录、应用 Authorization、身份头过滤、WebSocket、禁用／启用和管理退出验证。会修改这些专用入口的 enabled／revision，最终保留它们启用；不会停止、删除或修改原容器。测试 context 退出时清除其中的浏览器凭据。结果、截图保存在 `.local/release-0.7.1/`。

当前目标上基础域首段含连字符时，前缀请求会返回平台“应用未安装”页面。启动脚本在此情形保留只读观察；新的验证包使用无连字符的 `cofleetaccess`。原管理端没有迁移域名，两台旧设备保持连接。不能将 HTTP 200 的平台错误页面当成访问成功。
