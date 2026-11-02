# CoFleet 应用图标

文件：`cofleet-icon.png`，1254 × 1254，RGBA 透明背景。保留生成图原始像素与 alpha，用于项目 README 展示；LPK 使用等比缩小的 `cofleet-icon-256.png`（256 × 256），以满足图标小于 200 KiB 的打包提示。

生成方式：内置 ImageGen 工具，`transparent_background: true`。三块互相连接的电脑节点表达多设备协同；深绿、玉绿及薄荷色与当前项目配色一致。当前未替换托盘程序中的简易图形；应用图标与网页 favicon 已随新包部署并验证。

## 网页 favicon

`favicon.ico` 从原始 PNG 等比缩放导出，包含 16、32、48、64、128、256 像素六种尺寸，保留透明背景。管理端将 ICO 编译进二进制，提供 `/favicon.ico`，页面通过相对链接加载，因此部署不依赖外部图片服务。

复现命令（ImageMagick）：

```sh
magick assets/cofleet-icon.png -define icon:auto-resize=256,128,64,48,32,16 assets/favicon.ico
```

## 最终提示词

```text
Use case: logo-brand. Create one finished application icon for CoFleet (聚机), an open source lightweight app that makes multiple personal computers work together through a small central hub. Asset type: a single square desktop application icon, 1024x1024, clean transparent canvas outside the icon. Primary subject: three bold softly rounded rectangular computer-node tiles elegantly interlocking into one cohesive compact symbol, suggesting both a small fleet and a subtle open C silhouette. A small central negative-space junction implies coordination; avoid thin network diagram lines. Intentional minimal geometry with excellent silhouette and balanced negative space, recognizable at 32 pixels. Palette: deep forest green and jade green with a restrained mint highlight, matching a calm professional developer tool. Flat vector-like shapes with very subtle tonal depth, sharp clean edges, no photoreal materials. Centered large symbol with generous 12 percent safe margin; no outer app tile, no enclosing background square, no shadows outside the mark. One icon only, no grid, no mockup, no text or letters, no watermark, no tiny details. Output genuine transparent alpha around the symbol. This is a practical production app icon rather than a decorative illustration.
```

工具实际输出 1254 × 1254；原始提示词中的期望尺寸为 1024 × 1024。
