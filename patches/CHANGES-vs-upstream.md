# 相对上游的改动记录

`desktop/` 来自 BongoCat 桌面端（Apache-2.0），本仓库把它整棵树收进来并按「只驱动一块圆屏」重做。
这份文件记录改了哪些、为什么改，方便日后把上游的修复合回来。

| 日期 | 范围 |
| --- | --- |
| 2026-10-27 | 圆屏页、实时预览、开机提权、去掉桌面窗口设置 |
| 2026-10-27 | 帧改走 UDP 数据报、模型音效改由圆屏播放、屏端 Wi-Fi 高性能锁 |

## 新增

| 文件 | 作用 |
| --- | --- |
| `desktop/crates/bongocat-app/src/device_link.rs` | 产品侧的 `DeviceLink` 实现：把圆屏页的编辑落到 `MagneoHandle`，并把条带配置持久化到 `<config>/device-screen.json`（写临时文件再改名）。 |
| `desktop/crates/bongocat-ui/src/window/device_page.rs` | 圆屏页的视图状态与 `DeviceLink` trait。界面层不依赖发布端，只认这一个 trait，避免两个 crate 互相依赖。 |
| `android/java/com/bongocat/catscreen/Overlay.java` | 圆屏底部信息条的模型：带高、底色、若干行（模板/字号/颜色/对齐/字重/字体/中心 Y）。 |
| `android/res/values/strings.xml` | 设备端文案。 |
| `android/java/com/bongocat/catscreen/UdpFrameChannel.java` | 帧通道：每秒向主机播报一次自己的数据报端口，按 `channel_seq` 重组分片，缺片即丢弃。另有 `LINK_STATS` 回报收到的帧数与是否要求回退 TCP。 |
| `android/java/com/bongocat/catscreen/PublisherClient.java` | 地址不再是 `final`：`KIND_RELINK` 到达时改地址、写进 SharedPreferences、关掉旧 socket，重连循环下一轮就拨新地址。目标是回环地址时**不**开数据报通道——adb 反向隧道只转发 TCP，发给本机回环的 UDP 会被面板自己的协议栈吃掉，帧会静默停止而 TCP 计数器一切正常。 |
| `android/java/com/bongocat/catscreen/MainActivity.java` | 窗口亮度锁 1.0（`sbrt=1.0`）、`KEEP_SCREEN_ON` / `TURN_SCREEN_ON` / `DISMISS_KEYGUARD`，`root.setKeepScreenOn(true)`。窗口亮度替换系统曲线而不缩放它，这是平板不会在闲置几秒后自己调暗的唯一开关；`onResume` 里重设一次，因为退到后台会把显示权交回策略管理器。 |
| `android/java/com/bongocat/catscreen/AudioPlayer.java` | 把主机发来的音频库落到 `cacheDir/clips/clip-<id>`，用 `MediaPlayer` 预加载并按 id 播放；一条 clip 一个播放器，避免每次敲键都 prepare。 |
| `desktop/crates/bongocat-audio/src/remote.rs` | `MotionAudioSink` trait 与 `RemoteAudioBackend`：把「播这个文件」转发给圆屏，本机扬声器不再出声。`prepare` 不解码，只读字节。 |
| `desktop/crates/bongocat-app/src/audio_link.rs` | `ProductAudioSink`：音频服务先起来、发布端后创建，中间用这个可后挂的转发器接上。未挂载时全部调用为 no-op。 |
| `desktop/crates/bongocat-app/src/usb_link.rs` | adb 的封装：找 adb（配置 → `MIAOSCREEN_ADB` → PATH → `ANDROID_HOME`/`ANDROID_SDK_ROOT` → SDK 默认位置）、`adb reverse tcp:N tcp:N`、`am start -f 0x20000000` 带 `--es address`。全部带 `CREATE_NO_WINDOW`，否则每次调用都会在桌面上闪一个黑框。`am` 的失败写在 stdout 且退出码为 0，所以还要看输出里有没有 `Error:` |
| `scripts/dev/udp-probe.py` | 假主机，用来单独验证数据报通道与分片重组，不依赖产品。 |

## 修改

| 文件 | 改动 |
| --- | --- |
| `desktop/crates/bongocat-magneo/src/protocol.rs` | 增加 `0x14 OVERLAY`、`0x15 PREVIEW_CTRL`、`0x16 PREVIEW_FRAME`；`Overlay::default_strip()` 给出一条默认信息条；canvas 边界改为无条件发送（原先只在有背景图时发，桌面/键盘模式会被裁）。 |
| `desktop/crates/bongocat-magneo/src/sink.rs` | `Slot` 增加条带与预览状态、`MagneoHandle`（可克隆句柄）、连接时补发条带；`accept` 后的 socket 显式 `set_nonblocking(false)`（继承来的非阻塞会让每次写都返回 `WouldBlock`）；独立的读线程收快照，`MAX_INBOUND_MESSAGE = 4 MiB`。帧改走 UDP：控制端口 + 1 上绑数据报 socket，收到屏的播报后帧按 1180 字节切片发出，3 秒没播报就退回 TCP；`CLEAR` 在无帧期间每 400 ms 重发一次，因为丢一个 clear 会留下旧画面。`wait_after` 改成 200 ms 上限的等待，循环靠它重读 peer 与计时器。 |
| `desktop/crates/bongocat-app/src/main.rs` | 启动时先做一次提权（`runas`），同意后退出本进程、由提权副本接管；`--device-screen` 绑定端口；`--device-page-smoke`。 |
| `desktop/crates/bongocat-app/src/product_options.rs` | 新增 `--device-screen`（隐藏）、`--device-page-smoke`。 |
| `desktop/crates/bongocat-platform/src/startup_permission.rs` | `process_is_elevated()` / `relaunch_elevated()`（`ShellExecuteW` + `runas`）；Windows 的「权限流程」从打开文件夹改成重新以管理员启动。 |
| `desktop/crates/bongocat-ui/src/window/navigation.rs` | `ModelWindow` → `DeviceScreen`，位置保持第 4 项（索引 3）。 |
| `desktop/crates/bongocat-ui/src/window/render.rs` | 用圆屏页替换整块桌面窗口设置（窗口模式、置顶、穿透、缩放、透明度、背景色、最大帧率、悬停/空闲隐藏）；新增「应用与系统」里的更新分组删除。预览图用 `img(...).size_full().object_fit(Cover).rounded_full()` 画成圆形。 |
| `desktop/crates/bongocat-ui/src/window/{mod,view_state,smoke}.rs` | 圆屏页的字段、初始值与冒烟入口。 |
| `desktop/crates/bongocat-i18n/locales/*.json` | 8 种语言：`navigation.device_screen.title`、`settings.device_screen.*`、以及管理员权限提示的新措辞。 |

## 协议

| kind | 方向 | 载荷 |
| --- | --- | --- |
| `0x14` | 主机 → 屏 | `u32 版本 + Overlay`（带高、颜色、行模板） |
| `0x15` | 主机 → 屏 | `u32 版本 + u8 开关` — 要不要回传画面 |
| `0x16` | 屏 → 主机 | `u32 版本 + u32 长度 + JPEG` |
| `0x17` | 主机 → 屏 | `u32 版本 + u32 条数 + N×(u32 id + u32 长度 + 字节)` — 音频库，每个模型代际发一次 |
| `0x18` | 主机 → 屏 | `u32 版本 + u32 id + f32 音量` |
| `0x19` | 主机 → 屏 | `u32 版本` |
| `0x1A` | 屏 → 主机 | `u32 版本 + u64 完整帧数 + u64 数据报数 + u8 是否要求改走 TCP` |
| `0x1B` | 主机 → 屏 | `u32 版本 + u64 帧序号` — 每秒一个，屏靠它发现自己收不到数据报 |
| `0x1C` | 主机 → 屏 | `u32 版本 + u32 长度 + UTF-8 地址` — 换路。连接建立之后才发的才会生效：一条连接开始时取当前版本，所以连上之前排队的换路命令不会追着新连接再发一次 |

数据报是独立格式，不复用 `kind + length`：`"MCAT" + u8 类型 + u8 版本 + u16 保留`，其后按类型带 `nonce`（注册/应答）、`channel_seq + 总长 + 片序 + 片数 + 字节`（帧片）或 `channel_seq`（清除）。切片载荷 1180 字节，低于常见 MTU，避免 IP 分片。

模板占位符 `{HH} {mm} {ss} {yyyy} {MM} {dd} {W}` 在屏上展开，断链后时钟继续走，也不占带宽。

## 未做

- 圆屏页面里的条带设置改动即刻生效，但**字体**仍是屏端内置的两款（`NotoSansHans`、`FZLanTYJW`），桌面端没有做字体下发。
- 有线模式依赖 adb 服务常驻：`adb kill-server` 或拔线会丢掉反向隧道，这时页面上会显示「等待圆屏接入」，重新选一次「有线」就会重建。
- 音频库每次连接、每次换模型都全量重传。改成内容哈希寻址（主机发 manifest、屏回报缺哪些）后重连就只剩握手，属下一步。
- 音频服务在应用装配时就启动，而发布端要等窗口系统起来才存在，所以每次会话第一次 `set_library` 都早于 `attach`。`ProductAudioSink` 现在把这份库留到挂载时交接，否则启动后全程静音、要切一次模型才有声。
- 设备扬声器档位音量出厂是 1/15（默认档 8、扬声器档 1）。这个系统的音量按输出设备分别记档，音量键只改当时活跃的那个；必须在 `start()` 之后对齐，那时流才路由到扬声器。
- `hold_modifier`（长按修饰键才能交互）的整套录制控件仍留在 `bongocat-ui` 里，界面已经不再引用它，因此编译时有 dead-code 告警；删除它要连同 `window/tests/hold_modifier.rs` 一起处理，留作后续。
