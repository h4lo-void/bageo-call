# 手机设置

手机是通往电话网络的网关：它插着你的 SIM 卡，悄无声息地接起来电，把对方的声音发给主程序，再把 AI 的声音播放到通话里。它本身不做任何 AI 运算。

关于构建应用、它的权限和通信协议，全部写在 [`android/README.md`](../../android/README.md)（英文）里。这一页是从用户的角度来讲的。

## 支持哪些手机

- **实测过：Google Pixel 7、7a、8、9**，用 Magisk 或 KernelSU root。
- **其他手机：风险自负。** 应用需要手机的音频硬件层提供两样东西，而很多手机并不向应用开放，即使是特权应用也不行：在通话中录下对方的声音，以及把声音播放“进”通话里。只要缺一样，电话会被接起来，但有一方什么都听不到。目前还没有其他可用机型的列表——如果你试了，请反馈结果。
- Android 10 或更高版本。

有一项行为因手机而异，主程序会在每通电话开始时自行判断。如果手机的通话录音里不包含 AI 自己的声音（我们实际使用的 Pixel 9），对方可以在 AI 说话时插话，背景音也会贯穿整通电话。如果包含（我们实际使用的 Pixel 7 和 7a），AI 自己的声音会从它听到的内容里滤掉，所以它会先把一句话说完再继续听，背景音也只在 AI 说话时播放。

## 请用备用手机

安装模块之后，手机会变成一台专门接电话的机器：

- 应用会成为默认电话应用和来电过滤应用，Google 的电话应用会被停用（不会出现通话界面）；
- 勿扰模式会开启，铃声和通知音量调为零；
- 通话过程中麦克风静音、扬声器关闭、听筒音量调到最小——房间里的声音不会被听到，也不会有任何声音外放。

你仍然可以用另一个拨号应用从这部手机打电话出去，但别指望它像普通手机那样好用。以上这些都可以撤销：见[还原](#还原)。

## 安装

1. 给手机 **root**（Magisk 或 KernelSU）。这里不讲。
2. 如果你曾经把这个应用当作普通 APK 安装过，请先卸载。绝对不要两种同时存在。
3. 把 `pixelcallai_uplink-<version>.zip` 复制到手机上，打开 Magisk 或 KernelSU 应用 →“模块”（Modules）→“从本地安装”（Install from storage）→ 选择那个 zip → 重启。
4. 打开 **Myna**。按检查清单操作；每一行都有一个按钮：

   | 项目 | 要做什么 |
   |---|---|
   | 已安装为特权系统应用 | 重启后应该会显示正常。如果没有，说明模块没有生效。 |
   | 特权权限 | 由模块授予；不用点任何东西。 |
   | 应用权限 | “**授予权限**”（麦克风、电话、通话记录、通知）。 |
   | 默认电话应用／来电过滤应用 | “**设置**”。 |
   | “勿扰模式”权限 | 打开设置页面并允许。 |
   | 忽略电池优化 | 允许这项豁免，Android 才不会把服务停掉。 |

   Root 管理器询问时，请允许 root。
5. 让“**AI 接听**”保持开启。

## 连接到电脑

### USB（推荐）

1. 在手机上：“设置 → 关于手机 →”连点“版本号”七次；然后到“系统 → 开发者选项 →”开启 **USB 调试**。
2. 把手机插到运行主程序的那台电脑上。
3. 在手机上的“**允许 USB 调试吗？**”弹窗里点允许（勾选“一律允许”）。

这样就行了。主程序会找到手机，把应用的本地地址（`ws://127.0.0.1:8765`）通过 USB 线映射到自己这边，并启动应用的服务。它每十五秒检查一次，所以拔掉再插上、或者任何一边重启，都会自动恢复。USB 还能让手机保持充电，没有 Wi-Fi 也能用。

一台电脑可以插好几部手机；每部手机各自接听自己的号码。

### Wi-Fi

手机没办法用线连到电脑时，才用这种方式。

1. 启动主程序时，让它在网络上监听并要求令牌：在 `.env` 里设置 `AGENT_HOST=0.0.0.0`、`DEVICE_TOKEN=<a long random string>` 和 `CALLS_UI_TOKEN=<another one>`（两个各填一长串不同的随机字符；见[设置参考](SETTINGS.md)）。
2. 在应用的“连接”一栏里输入 `ws://<computer's address>:8787/ws?token=<DEVICE_TOKEN>`（其中 computer's address 换成电脑的地址），然后保存。“**测试连接**”会告诉你能不能连上。

这个连接没有加密（`ws://`）：只能在你自己的网络里使用，或者在前面架一个 TLS 反向代理并改用 `wss://`。通话走 USB 更稳定。

## 日常使用

- 服务运行时，应用会显示一条不打扰的常驻通知；重启后也会自动启动。
- 想暂停接听，可以在应用里关掉“**AI 接听**”（电话会静音响铃、不会被接起），或者在网页控制台里关掉“由 AI 接听来电”（电话会被接起来再挂断）。
- 你自己用这部手机拨出去的电话，应用不会介入。

## 还原

1. 在 Magisk / KernelSU 里停用或移除 **Myna Uplink** 模块，然后重启。如果手机启动不到那一步，请使用 root 管理器的安全模式，或者在 recovery 里以 root 身份运行：`touch /data/adb/modules/pixelcallai_uplink/disable`。
2. 移除模块**不会**把 Google 的电话应用恢复回来，也不会关掉勿扰模式——这些 Android 都会记住。重新启用电话应用：

   ```text
   adb shell su -c "pm enable com.google.android.dialer"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallServiceImpl"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallActivity"
   adb shell su -c "cmd role add-role-holder android.app.role.DIALER com.google.android.dialer"
   adb shell su -c "telecom set-default-dialer com.google.android.dialer"
   ```

3. 到系统设置里关掉勿扰模式，并把音量调回来。

## 不能用的时候

请看[故障排查](TROUBLESHOOTING.md)，以及 [`android/README.md`](../../android/README.md#troubleshooting)（英文）末尾的表格。
