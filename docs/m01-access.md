# M0.1：自动前缀入口与认证隔离

日期：2026-11-04（Asia/Shanghai）。结论：**用户已明确选择受信任应用模式；0.7.1 已实现并部署自动前缀入口。共享父域的完整 Cookie 隔离仍不成立。**

本轮沿用用户要求的自动域名，不要求用户自备域名。原管理中心已升级 0.7.1，原有设备连接地址、开发空间和应用保持。原 M0 与 HTTP 测试容器保持；新增独立访问验证工作台及一个仅启用观察的临时 Agent。

## 先区分两个目标

| 目标 | 结果与依据 |
| --- | --- |
| 自动生成前缀域名、平台 TLS、路由到服务 | 原 M0 已通过，本轮再次通过真实基础域和 static 前缀域访问 |
| 应用不能伪造／覆盖 CoFleet 的 host-only 认证 Cookie | 真实前缀域名下的合成 `__Host-` Cookie 检查通过 |
| 一次性授权、防重放、浏览器绑定、Host／revision／管理会话绑定 | 本机原型通过，现已集成 0.7.1 Manager |
| 应用完全不能向管理来源写入任何 Cookie | **不成立**：应用脚本仍能写共同父域的普通 Cookie |
| 微服平台会话不会受到任意应用父域 Cookie 的影响 | **尚无证明**：未操作真实平台令牌，不能据此声称平台凭据已被窃取或可伪造 |

不能把后两项改写成“已解决”。HttpOnly 保护脚本读取，`__Host-` 约束 CoFleet 自有 Cookie；两者都不会把共享注册域变成两个独立 Site。[MDN Cookie 规则](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Set-Cookie)、[Site 定义](https://developer.mozilla.org/en-US/docs/Glossary/Site)。

## 建议的自动入口结构

保留懒猫自动域名机制：管理端为 `<实际应用子域>.<微服根域>`，应用为 `e-<稳定ID>-<实际应用子域>.<微服根域>`。实际子域从 `LAZYCAT_APP_DOMAIN` 获取，不拼猜测值。入口 ID 稳定，改显示名称不改链接。

这里的前缀与基础域处于同一 DNS 标签；当前 M2 的 `e-<ID>.<access-domain>` 是独立访问域模式，不能只把 access-domain 设置成现有应用域就当作自动前缀模式。落地前需要新增明确的域名模式及已存 public_url 的迁移／兼容策略，测试实际 Host、路径和 TLS。旧的隔离域模式保留，不允许静默降级。

只有已知管理 Host 才能进入管理路由；只有查库匹配的应用 Host 才能进入代理。未知 Host、应用 Host 上的管理 API／Agent WebSocket／设备控制接口一律拒绝。应用 Host 不得回退到管理路由。不要按客户端提供的任意 Host 生成回跳 URL。

## 同 Site 下的加固流程（0.7.1 已集成）

1. 管理端先通过可信懒猫 ingress 验证当前真实用户，再签发 `__Host-cofleet_manager`：Secure、HttpOnly、Path=/、SameSite=Lax、无 Domain。入口 API 变更同时要求平台身份、该管理会话、精确 Origin 和请求标记；授权表单另用绑定浏览器的 challenge 防重放；平台 UID 与会话 UID 不一致时拒绝并要求重新确认登录，不自动切换账号。
2. 访问应用 Host 时，由网关的保留路径建立 `__Host-cofleet_bootstrap` 并产生随机 challenge。必须先有目标浏览器的 bootstrap，不能拿到一个访问链接就把其他浏览器登录成票据所有者。
3. 管理端在已认证的同源操作中核验 challenge 所属 endpoint、owner、设备、当前 Agent 会话与 revision。签发至少 256 位随机的一次性票据，服务端仅存摘要；TTL 30 秒。限制每会话、每入口与全局数量，过期清理，拒绝重放与重复签发。目标地址来自服务端的入口配置。
4. 票据通过顶层 POST 表单交给目标 Host 的保留兑换路径，不置于查询参数、历史记录、日志或代理请求。兑换同时核验 Origin、bootstrap、Host、管理会话仍有效及 revision。成功后清 bootstrap，签发 `__Host-cofleet_access`，该会话只对应一个用户、入口、Host、revision 和 Agent 会话，15 分钟过期。
5. HTTP 与 WebSocket 握手同时核验平台可信身份和入口会话，不允许仅凭平台 Header 或仅凭访问 Cookie 授权。平台 Cookie／身份头、CoFleet Cookie／授权路径不得送到应用。应用自己的 Cookie、Authorization、登录和 WebSocket 保留。
6. 禁用／删除入口、revision 变更、Agent 换会话、管理端退出必须撤销相关 grants、会话和活跃流。管理端重启默认使内存授权失效，持久入口配置保留。平台端退出后的新握手重新验证平台身份；已有流的撤销延迟、租约期限和可用的服务端撤销事件必须另行实测，不能声称瞬间失效。
7. 管理授权响应 `Cache-Control: no-store`。票据不出现在 URL，授权跳转页只使用受控模板；本轮实测 POST 表单需要有效 Origin，使用 `Referrer-Policy: strict-origin`，不能一边设置 no-referrer 导致 Origin=null，一边声称严格校验 Origin 已通过。策略只发送来源，不发送路径或票据。拒绝 Origin=null，禁止宽泛 CORS。

0.7.1 已将上述流程接入真实平台身份和 Agent 隧道，并在服务端以 5 秒周期复核流的会话、入口版本及 Agent 会话。受保护响应统一 no-store，防止浏览器以普通 HTTP 缓存绕过后续访问校验；不能撤回已下载内容或应用自行保存的离线内容。票据表、会话表有全局容量上限和过期清理。深链接首次授权后目前回到应用根路径。

## 实测记录

可复现工具：[tools/m01/README.md](../tools/m01/README.md)。脱敏结果：[m01-access-results.json](m01-access-results.json)。

### 本机 HTTPS：17 项检查通过

管理与应用使用 `manager.cofleet.test`、`one.cofleet.test`、`two.cofleet.test`，故意共享注册域。临时浏览器解析规则映射到 loopback，不修改系统配置。只有隔离的测试 context 忽略本机一次性自签名证书错误；不代表生产 TLS 校验通过。

通过：管理会话属性；复制票据到无 bootstrap 的浏览器被拒；重复签发被拒；真实浏览器表单兑换及访问；会话不发给兄弟主机；复制会话到错误 Host 被拒；票据重放被拒；父域伪造 `__Host-` 被拒；普通父域污染复现；脚本不能读 HttpOnly 会话；跨来源不能读管理响应；应用来源不能变更管理状态；应用 Host 无管理接口；模拟第二用户不能授权他人入口；revision 变更撤销会话；管理退出撤销已有会话和未兑换票据。

夹具身份只有 alice／bob，**不是真实跨微服账号测试**。票据与会话 TTL 已设置并清理过期项，但过期边界、时钟故障、容量压力、WebSocket 与活跃流撤销尚未在这个夹具验证。

### 真实懒猫前缀域：4 项检查通过

在原 M0 基础域与 static 前缀域验证正常 TLS／平台访问、合成 host-only 会话不发送到兄弟 Host、父域不能伪造 `__Host-` Cookie、HttpOnly marker 不可被脚本读取。

这里的 Cookie 是通过测试浏览器注入的无害合成值，**不是生产网关已实现签发／兑换**。普通父域 marker 仍能出现在管理 Host；测试后清除并销毁浏览器 context。真实平台凭据只在内存中用于登录，未修改、尝试覆盖或输出。

规范校准：lzcos `1.6.2+fix.4`、lzc-cli `2.0.9`，官方多域名、路由、身份 Header、public_path、OIDC 五页均在线读取成功。无新增平台字段；本地快照与使用到的自动前缀语义没有发现冲突。原文摘要见 [校准记录](deployment/m01-calibration.json)；公开记录移除了来源 URL 查询参数。

## 部署选择与放行条件

**选择 A：保持原先的完整 Cookie 隔离目标。** 自动前缀域名自身无法满足，需要平台提供隔离的注册域／公共后缀边界，或经过验证的独立访问域与 TLS／可信身份传递。当前没有证据证明目标微服能自动分配这种隔离 Site。不能通过配置 `COFLEET_ACCESS_DOMAIN` 为现有父域来解除限制。

**选择 B：采用“用户主动添加的受信任应用”模式。** 沿用平台共享父域的信任边界，落实上述自有会话加固，允许用户主动添加明确受信任的应用。它可以保留自动前缀体验，但不能承诺任意应用隔离；恶意应用的父域 Cookie 污染、平台登录可用性影响仍是残余风险。现有“任意应用不能设置管理来源 Cookie”的要求需明确修订，不能默默降低标准。用户已在本次会话明确选择此模式，0.7.1 已实现并用于独立验证工作台。

本轮选择 B，不宣称达到了选择 A。真实第二账号、平台端退出与长时负载仍待补充；模拟账号结果不能替代它们。M1 不受影响。


## 0.7.1 部署与平台路由限制

实测目标系统上，原基础域首段 `cofleet-test` 的前缀请求被平台拦在应用前，返回“应用未安装”（平台错误页甚至返回 HTTP 200，不能仅凭状态码判成功）；同设备上无连字符的 `cofleetm0…` 和 `cofleetaccess` 能正确接收前缀。对比测试覆盖 static 前缀、单段 ID 前缀及 e-ID 前缀。不将此结论泛化为所有懒猫版本的规则。

原管理端的域名关系到两台现有 Agent，未重命名或重新初始化。启动脚本对首段含连字符的现有部署保留观察模式；独立域模式仍可显式配置。自动访问验证使用独立包 `xyz.mxue.lzc.app.cofleet.access`、无连字符基础域 `cofleetaccess`，源配置见 [deploy/lazycat-access](../deploy/lazycat-access)。验证节点仅用 `--docker-observe`，不启用 Docker 执行器、开发空间或构建，不接管任何已有应用。

现有域名迁移需另行安排设备连接兼容与回滚，不在本轮悄悄改变。验证工作台仅列出专门连接到它的节点，不能误称原有两台设备已迁移。

本轮还修复动态宿主端口读取：对于运行中的容器读取 `NetworkSettings.Ports`，而不是配置中可能为空的 `HostConfig.PortBindings.HostPort`。这使外部工具以随机发布端口运行的现有服务可以被发现和重新核验。

### 验收命令

- `cargo test --locked --lib --test ws_hub`：31 项单元测试、5 项协议集成测试。
- `cargo clippy --locked --all-targets -- -D warnings`。
- `tests/manager_ui.mjs`：信任确认前禁止创建、自动刷新、日志与导航回归。
- `tests/scheduler_ui.mjs`、`python3 tests/smoke.py`。
- 本机及真实 Cookie 实验：`tools/m01/browser.mjs`、`tools/m01/platform.mjs`。
- 真实前缀工作台访问闭环：`COFLEET_M01_MANAGER_URL=<实际验证工作台URL> node tools/m01/live.mjs`。此脚本只创建／复用 HTTP 测试和 M0 夹具入口，检查应用登录、头过滤、WebSocket、禁用、再授权和退出撤销，最后保留验证入口启用。需要先运行原 M0 夹具与 HTTP 测试容器，真实凭据只在内存中使用。

按现有 0.7.0 状态目录直接升级，入口表不重置；新增信任记录为可重复创建的独立表，只在所属用户明确确认后写入。入口删除同时清理信任记录。没有修改现有环境变量、认证凭据或旧 Agent 配置。


### 本轮线上结果

真实验证工作台完成 17 项检查：明确信任后创建、自动前缀 TLS／平台身份、浏览器绑定授权、HTTP 页面及 health、host-only Cookie、管理 Cookie 不发送到应用、应用 Host 不进入管理 API、禁用拒绝新访问、重新启用要求新授权、管理退出撤销、无浏览器错误，以及应用 Cookie 登录、Authorization 保留、身份与网关 Cookie 过滤、WebSocket echo、退出关闭活跃 WebSocket。结果见脱敏 JSON 中的 `trusted_mode_release`；原始 URL、浏览器凭据和私有夹具密码不入库。

同一轮升级后，旧管理端 HTML／CSS／JS 与源码一致，两台原在线设备重连，原预留配置、开发空间卷／路径／意图、当前及上一发布版本保持；编辑器、项目预览、已发布应用访问均通过，匿名设备访问被拒绝。

验证工作台与专用观察 Agent 保留在线供体验，不设开机启动。原 M0 夹具、HTTP 容器和其数据未改动。关闭体验环境时，先停止 `.local/release-0.7.1/workbench/agent.pid` 对应且 cmdline 包含该节点目录的专用 Agent，再仅卸载 `xyz.mxue.lzc.app.cofleet.access`；不得卸载原管理中心或删除原容器。
