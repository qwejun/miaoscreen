' 喵屏 MiaoScreen 启动器
' 双击即可。优先用 release 构建：它是 GUI 子系统程序，本身没有控制台窗口，
' 所以不需要靠隐藏窗口来遮丑，界面也不会被一起藏起来。
' 参数 --device-screen 指定圆屏 sink 的监听地址，缺了它程序会退化成桌面窗口模式。
Option Explicit
Dim shell, fso, root, exe, candidates, i
Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
root = fso.GetParentFolderName(WScript.ScriptFullName)

If fso.FileExists(root & "\desktop\target\release\bongocat-app.exe") Then
    exe = root & "\desktop\target\release\bongocat-app.exe"
    shell.CurrentDirectory = root & "\desktop"
Else
    root = "D:\catscreen"
    If fso.FileExists(root & "\desktop\target\release\bongocat-app.exe") Then
        exe = root & "\desktop\target\release\bongocat-app.exe"
    Else
        exe = root & "\desktop\target\debug\bongocat-app.exe"
    End If
    shell.CurrentDirectory = root & "\desktop"
End If

If Not fso.FileExists(exe) Then
    MsgBox "找不到 bongocat-app.exe：" & exe, 16, "喵屏 MiaoScreen"
    WScript.Quit 1
End If

' 1 = 普通窗口。GUI 子系统程序没有控制台，所以这里不会闪黑框。
shell.Run """" & exe & """ --device-screen 0.0.0.0:7391", 1, False
