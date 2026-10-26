# CatScreen

把 Live2D 猫显示到 USB 连接的圆屏（MAGNEO C110001）上，电脑桌面不显示猫；键鼠和手柄仍然驱动猫的反应。

本仓库是独立的圆屏显示项目，不复用也不修改桌面版 BongoCat 的应用层；它复用 BongoCat 仓库里的
runtime、模型和输入 crate，模型资源也直接读取那边的 `resources/models`。

## 结构

```text
publisher/   Rust 发布端：无窗口，读取 PC 输入，把每帧编码后通过 TCP 发出
android/     Android 客户端：解析协议，用 GLES2 在圆屏上渲染
scripts/     构建与启动脚本
scripts/dev/ 调试用探针（读窗口、截屏、只看协议不渲染）
signing/     测试签名密钥（不入库）
```

## 依赖

- 上游 BongoCat 检出放在本仓库旁边：`../kat`（默认路径，可用环境变量覆盖）。
  - Rust crate 直接按路径引用 `../kat/crates/*`；
  - 模型默认读 `../kat/resources/models`（`CATSCREEN_MODELS` 可覆盖）；
  - Cubism Core 的 DLL 默认取 `../kat/vendor/cubism`（`CATSCREEN_VENDOR` 可覆盖）。
- Rust 1.97.1。发布端用 GNU 工具链编译（`x86_64-pc-windows-gnu`），需要一份 mingw `gcc`。
- Android 构建用 SDK 的 `aapt`、`d8`、`apksigner` 和 JDK 的 `javac`，不使用 Gradle，不联网。
  默认路径写在 `scripts/build-apk.ps1` 参数里，机器不同就传参覆盖。

## 运行

```powershell
# 构建 APK（首次会生成 signing/kat.keystore）
.\scripts\build-apk.ps1

# 编译发布端并启动整条链路
.\scripts\start.ps1 -Build

# 只切模型（已编译过）
.\scripts\start.ps1 -Model keyboard

# 全部停止
.\scripts\start.ps1 -Stop
```

`start.ps1` 会做四件事：检查设备在线、建立 `adb reverse tcp:7391 tcp:7391`、安装 APK、启动发布端
和设备端 Activity。发布端先用 TCP 连上再发帧，断开后设备会自己重连，USB 掉了重新跑一次脚本即可。

## 协议

`publisher/src/protocol.rs` 与 `android/java/com/bongocat/catscreen/Protocol.java` 是同一份格式的
两端实现：小端、`kind: u8` + `length: u32` + payload，payload 以协议版本开头。

| kind | 名称 | 作用 |
| --- | --- | --- |
| 0x10 | HELLO | 连接后的第一条，版本校验 |
| 0x11 | RESOURCES | 纹理、背景、键位素材；模型换代时重发 |
| 0x12 | FRAME | 一帧的 drawable、顶点、索引、启用的键 |
| 0x13 | CLEAR | 清空画面（隐藏模型） |

设备端不保存模型状态：收到新 generation 的 RESOURCES 就重建纹理，其余时间只画最新的一帧，
所以慢一帧只是丢一帧，不会累积延迟。

## 现状与限制

- 圆屏 800×800、57.6 Hz，当前实测约 42 fps（`adb logcat -d -s CatRenderer` 每秒一行）。
- 没有设置界面：模型用命令行参数切换，配置不落盘。
- 只支持 USB + `adb`：电脑侧要能跑 `adb reverse`，设备侧需要 `INTERNET` 权限。
- 需要 Windows：发布端用的是 BongoCat 的 `WindowsInputService`（Raw Input）。
