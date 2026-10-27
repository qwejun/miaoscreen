# 相对上游的改动记录

`desktop/` 来自 BongoCat 桌面端（Apache-2.0），本仓库把它整棵树收进来并按「只驱动一块圆屏」重做。
这份文件记录改了哪些、为什么改，方便日后把上游的修复合回来。

| 日期 | 范围 |
| --- | --- |
| 2026-10-27 | 圆屏页、实时预览、开机提权、去掉桌面窗口设置 |

## 新增

| 文件 | 作用 |
| --- | --- |
| `desktop/crates/bongocat-app/src/device_link.rs` | 产品侧的 `DeviceLink` 实现：把圆屏页的编辑落到 `MagneoHandle`，并把条带配置持久化到 `<config>/device-screen.json`（写临时文件再改名）。 |
| `desktop/crates/bongocat-ui/src/window/device_page.rs` | 圆屏页的视图状态与 `DeviceLink` trait。界面层不依赖发布端，只认这一个 trait，避免两个 crate 互相依赖。 |
| `android/java/com/bongocat/catscreen/Overlay.java` | 圆屏底部信息条的模型：带高、底色、若干行（模板/字号/颜色/对齐/字重/字体/中心 Y）。 |
| `android/res/values/strings.xml` | 设备端文案。 |

## 修改

| 文件 | 改动 |
| --- | --- |
| `desktop/crates/bongocat-magneo/src/protocol.rs` | 增加 `0x14 OVERLAY`、`0x15 PREVIEW_CTRL`、`0x16 PREVIEW_FRAME`；`Overlay::default_strip()` 给出一条默认信息条；canvas 边界改为无条件发送（原先只在有背景图时发，桌面/键盘模式会被裁）。 |
| `desktop/crates/bongocat-magneo/src/sink.rs` | `Slot` 增加条带与预览状态、`MagneoHandle`（可克隆句柄）、连接时补发条带；`accept` 后的 socket 显式 `set_nonblocking(false)`（继承来的非阻塞会让每次写都返回 `WouldBlock`）；独立的读线程收快照，`MAX_INBOUND_MESSAGE = 4 MiB`。 |
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

模板占位符 `{HH} {mm} {ss} {yyyy} {MM} {dd} {W}` 在屏上展开，断链后时钟继续走，也不占带宽。

## 未做

- 圆屏页面里的条带设置改动即刻生效，但**字体**仍是屏端内置的两款（`NotoSansHans`、`FZLanTYJW`），桌面端没有做字体下发。
- `hold_modifier`（长按修饰键才能交互）的整套录制控件仍留在 `bongocat-ui` 里，界面已经不再引用它，因此编译时有 dead-code 告警；删除它要连同 `window/tests/hold_modifier.rs` 一起处理，留作后续。
