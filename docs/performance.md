# Linux 原型资源快照

测量日期：2026-11-02。原始数据见 [performance-linux.json](performance-linux.json)。

这是 **30 秒短时采样**，不是 10 分钟以上稳定性基线，也不代表 Windows、macOS 或 LC 02 的占用。

环境：Linux x86_64，Rust 1.99.1，release 构建；单节点，心跳周期 5 秒，预热 5 秒。管理端、Agent、托盘同时运行，控制通道使用回环 WebSocket，不包含浏览器、TLS 连接、任务负载和容器运行时。

| 进程 | 平均 RSS | 平均 PSS | 平均私有页 |
| --- | ---: | ---: | ---: |
| 管理端 | 4.47 MiB | 2.07 MiB | 2.06 MiB |
| Agent | 5.15 MiB | 2.76 MiB | 2.75 MiB |
| 托盘 | 29.92 MiB | 13.63 MiB | 12.33 MiB |

RSS 包含共享页，不能将各进程 RSS 之和视为实际独占内存。PSS 将共享页按共享进程数分摊，受当前桌面中其他进程影响。托盘目前使用 GTK/AppIndicator，资源占用明显高于无界面的 Agent；退出托盘后后台 Agent 可继续运行。

本次 CPU 时间差在采样精度下为 0.000%，不能解释为完全没有 CPU 消耗。采样期间 Agent RSS 增加 4 KiB、托盘增加 192 KiB；短时间变化不足以判断是否存在长期增长，需继续执行至少 600 秒采样以及长时间重连测试。

复现方式见 [开发文档](development.md)。测试脚本不会安装服务，结束后清理自己启动的进程和临时配置。

改为 WebSocket 前的 [HTTP 心跳版快照](performance-linux-http.json) 留作历史参考；两次为独立短时采样，不是受控性能对照实验。
