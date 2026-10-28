# MiaoScreen（喵屏）

把 Live2D 猫显示到一块 USB / Wi-Fi 连接的圆形屏幕（800×800）上，桌面端负责换模型、调设置，
键鼠和手柄的输入实时驱动屏幕里的猫。

这是一个自用的独立项目，围绕一款特定的小圆屏硬件打造，不追求通用性。

## 下载

不打算自己编译就看 [Releases](https://github.com/qwejun/miaoscreen/releases/latest)：

- `MiaoScreen-*-windows-x64.zip` — PC 端。解压到任意目录，双击 `MiaoScreen-*.exe`。
  压缩包里的 `resources/models/` 和 `vcruntime140.dll` 必须和 exe 放在一起，缺了就打不开。
- `CatScreen-*-android.apk` — 圆屏端，`adb install -r`，或拷到设备上直接点安装（需允许未知来源）。

PC 端要求 Windows 10 以上（用 D3D11 + DirectWrite）。不需要装 .NET，也不需要管理员权限。
安卓端 `minSdkVersion 22`，Android 5.1 以上。

## 版本号

硬盘上同时躺着几个版本时，靠这几处分辨：

| 位置 | 样子 |
| --- | --- |
| 文件 | `MiaoScreen-0.1.1.exe` |
| 设置窗口标题栏 | `喵屏设置 0.1.1` |
| 托盘图标悬停 | `喵屏 0.1.1` |
| exe 属性 → 详细信息 | 文件版本 `0.1.1`、产品名 `MiaoScreen` |
| 设置 → 关于 | `MiaoScreen` / `版本 0.1.1` |

版本号只有一个来源：`desktop/Cargo.toml` 的 `[workspace.package] version`
（Windows 资源里的 `FILEVERSION` 由 `build.rs` 从 `CARGO_PKG_VERSION` 注入，窗口标题和托盘
提示读同一个常量）。设备端是 `android/AndroidManifest.xml` 的 `versionName` / `versionCode`，
两者互不依赖，发版时一起改。

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

## 皮肤

模型库里除了自带的「标准 / 键盘 / 手柄」三只猫，还有 62 个来自
[Awesome-BongoCat](https://github.com/ayangweb/Awesome-BongoCat) 的第三方皮肤，
共 55 个角色（阮梅三模式齐全，另有几个角色带键盘或手柄版本）。

这些目录体积合计 640MB，**不随仓库分发**——它们让 clone 变成一次 640MB 的下载，
而这 640MB 全是不可编译的模型数据。仓库里的 `.gitignore` 保留三个自带模型，
跳过其余目录；本地重建：

```bash
python scripts/fetch-skins.py          # 下载 + 解压到 desktop/resources/models/
python scripts/fetch-skins.py --list   # 只打印 id/标题/模式表，不下载
```

脚本从合集仓库取中文包，按字节大小与英文包配对来确定 ASCII id（合集把同一份
模型发布了中英两套，逐字节相同）。目录名就是模型 id，只允许 `[A-Za-z0-9._-]`，
所以中文名放在 `desktop/crates/bongocat-app/src/model_listing.rs` 的
`PRESET_MODELS` 表里作为显示名。加新皮肤要同时改脚本能产出的那张表，`--list`
打印的就是可以直接粘贴的格式，两者由测试钉住一致。

各个皮肤的版权归原作者，模型与角色形象的权利归其各自权利人（多为游戏厂商）。
本仓库只做分发渠道，使用时请遵守原作者的授权条件。

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
| 0x15 | PREVIEW_CTRL | 要不要回传画面（改设置或换模型时才请求一帧，不常开） |
| 0x16 | PREVIEW_FRAME | 圆屏回传的 JPEG 快照，桌面端画成圆形预览 |
| 0x17 | AUDIO_LIBRARY | 模型音效库一次性下发，设备端落盘缓存 |
| 0x18 | AUDIO_PLAY | 播哪个音效，按 id 引用 |
| 0x19 | AUDIO_STOP | 停止播放 |
| 0x1A | LINK_STATS | 设备端每秒上报：收帧数、丢片数、解码耗时 |
| 0x1B | FRAME_MARK | 空闲心跳，1 秒一次，画面不动也不掉线 |
| 0x1C | RELINK | 换地址：有线/无线切换时通知设备端起新连接 |

帧数据不走 TCP：`0x12` 的 payload 拆成 1180 字节切片，各自塞进独立的 UDP 数据报
（`"MCAT" + type u8 + version u8 + reserved u16`，type 1 REGISTER / 2 ACK / 3 FRAME_SLICE / 4 CLEAR），
UDP 端口 = TCP 端口 + 1。设备端按 `channel_seq` 重组，缺片直接弃掉这一帧，不等不重传。

设计要点：桌面端只发几何与模板，不栅格化；时钟占位符（`{HH}:{mm}` 等）由设备端用本地时钟
展开，断链后时间照走；设备端无状态，慢一帧只是丢一帧，不累积延迟。音频同样不实时推流——
音效在连接时下发一次，播放由设备端本地完成，链路抖动不影响声音。

## 构建

- Rust 1.97（发布端用 GNU 工具链，需要 mingw `gcc`）。
- Android：SDK 的 `aapt`、`d8`、`apksigner` + JDK 17 的 `javac`，不用 Gradle，不联网。
  路径写在 `scripts/build-apk.ps1` 参数里，机器不同传参覆盖。

## 许可

`desktop/` 衍生自 BongoCat，遵循 [Apache-2.0](desktop/LICENSE)；本仓库其余部分同样以
Apache-2.0 发布。
