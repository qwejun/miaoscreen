# MiaoScreen（喵屏）

把 Live2D 猫显示到一块 USB / Wi-Fi 连接的圆形屏幕（800×800）上，桌面端负责换模型、调设置，
键鼠和手柄的输入实时驱动屏幕里的猫。

这是一个自用的独立项目，围绕一款特定的小圆屏硬件打造，不追求通用性。

## 结构

```text
desktop/     桌面客户端（Rust + GPUI）：设置界面、模型库、托盘、输入采集
publisher/   无窗口发布端（Rust CLI）：读输入、编码帧、TCP 推流
android/     圆屏设备端（纯 Java + GLES2，无 Gradle）：解析协议、渲染
scripts/     构建与启动脚本
scripts/dev/ 调试探针（读协议流、截屏）
patches/     对 desktop 的改动备份与说明
```

`desktop/` 基于 [BongoCat](https://github.com/ayangweb/BongoCat)（Apache-2.0）改造：
圆屏相关的 sink 与协议位于 `desktop/crates/bongocat-magneo/`。
上游在桌面窗口上做的那套设置（窗口模式、置顶、穿透、缩放、透明度、最大帧率、悬停/空闲隐藏）
对本项目没有意义，界面已经删掉，其中「桌面窗口」那一页换成了「圆屏」页。
相对上游的完整改动记录在 `patches/CHANGES-vs-upstream.md`。

## 运行

```powershell
# 桌面客户端（带界面），--device-screen 指定监听地址
.\desktop\target\debug\bongocat-app.exe --device-screen 0.0.0.0:7391

# 无窗口发布端（命令行），可配信息条
.\target\debug\miaoscreen.exe --model standard --address 0.0.0.0:7391 `
    --banner "今天也要加油" --clock "{HH}:{mm}"

# 设备端默认连 127.0.0.1:7391（配 adb reverse）；Wi-Fi 直连时：
adb shell am start -n com.bongocat.catscreen/.MainActivity --es address 192.168.x.x:7391

# 构建设备端 APK
.\scripts\build-apk.ps1

# 一键启动（USB + adb reverse 场景）
.\scripts\start.ps1 -Build
```

## 协议

`publisher/src/protocol.rs` 与 `desktop/crates/bongocat-magneo/src/protocol.rs` 是同一份格式的
两份一致实现；设备端实现在 `android/java/com/bongocat/catscreen/Protocol.java`。
小端、`kind: u8` + `length: u32` + payload，payload 以协议版本开头。

| kind | 名称 | 作用 |
| --- | --- | --- |
| 0x10 | HELLO | 连接后的第一条，版本校验 |
| 0x11 | RESOURCES | 纹理、背景、键位素材；模型换代时重发 |
| 0x12 | FRAME | 一帧的 drawable、顶点、索引、启用的键 |
| 0x13 | CLEAR | 清空画面（隐藏模型） |
| 0x14 | OVERLAY | 底部信息条：时间/日期/标语的模板与排版 |
| 0x15 | PREVIEW_CTRL | 要不要回传画面（圆屏页打开时开启） |
| 0x16 | PREVIEW_FRAME | 圆屏回传的 JPEG 快照，桌面端画成圆形预览 |

设计要点：桌面端只发几何与模板，不栅格化；时钟占位符（`{HH}:{mm}` 等）由设备端用本地时钟
展开，断链后时间照走；设备端无状态，慢一帧只是丢一帧，不累积延迟。

## 构建

- Rust 1.97（发布端用 GNU 工具链，需要 mingw `gcc`）。
- Android：SDK 的 `aapt`、`d8`、`apksigner` + JDK 17 的 `javac`，不用 Gradle，不联网。
  路径写在 `scripts/build-apk.ps1` 参数里，机器不同传参覆盖。

## 许可

`desktop/` 衍生自 BongoCat，遵循 [Apache-2.0](desktop/LICENSE)；本仓库其余部分同样以
Apache-2.0 发布。
