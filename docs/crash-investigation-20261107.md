# 2026-11-07 停止响应排查

## 当前结论

在用户补充“迷你模式选中窗口后无响应”并允许隔离复现后，已于 v2.1.5 的 40 DIP 极窄迷你列表复现高 CPU 无响应。两份相隔约两分钟的真实调用栈将该次现场定位到原生截断名称提示的反复测量／定位流程，涉及 `LVS_EX_LABELTIP` 与列表自绘回调。

v2.1.6 已显式清除迷你列表的 `LVS_EX_LABELTIP`，保留原列表选择、双击、滚轮、双缓冲及普通模式行为。同条件下已观察到进入／返回调节页、悬停及键盘调整正常。用户随后按 Esc 停止电脑控制，完整回归和测试环境清理没有完成；结论仅覆盖本次复现路径，不能把下列四次历史 Hang 全部归为同一原因。

## 初始只读排查阶段

本机 Windows Application 日志和可靠性记录均存在 4 次 Trans 停止响应记录，事件类型为 `Application Hang / 1002`。记录证明程序曾停止响应并被关闭，但没有历史阻塞调用栈，因此初始阶段尚不能确定原因。

| 时间（北京时间） | 版本 | 进程 ID | 发布目录 |
| --- | --- | --- | --- |
| 2026-11-07 13:51:46 | 2.1.3.0 | 56452 | `dist/release-20261107-133735` |
| 2026-11-07 13:55:59 | 2.1.3.0 | 51676 | `dist/release-20261107-133735` |
| 2026-11-07 14:28:47 | 2.1.4.0 | 6976 | `dist/release-20261107-140645` |
| 2026-11-07 14:37:02 | 2.1.4.0 | 32732 | `dist/release-20261107-140645` |

初始查询过去 30 天的相关提供程序记录时，未找到 Trans 的 `Application Error / 1000` 或 WER 异常报告；已检查的用户 CrashDumps 与 WER 队列／归档中未找到对应历史转储。这不证明程序从未发生其他异常。

排查时运行的是 `dist/release-20261107-143817/Trans.exe`（v2.1.5，PID 2748），进程查询显示正常响应，当前状态不能反推此前阻塞原因。

14:48:26 的只读 WCT 快照复核了该进程路径、EXE 哈希和主窗口 TID 59488；结果只有主线程一个节点，状态为 `Blocked`，`cycle=false`，没有报告 SendMessage／COM 依赖节点。单节点 Blocked 可以是正常等待消息，不作为卡死证据，亦不能解释历史 Hang。该检查未暂停线程、发送窗口消息或操作界面。

## 初始静态审查结果

- 主窗口、设置面板、列表及绘制路径未发现可高置信解释本次 Hang 的无限循环、RefCell 重入 panic 或释放后使用。布局有合并和重入保护；列宽调整最多三轮；窗口图标查询有总计 30 ms 的超时预算
- 容器回调与控制器使用 Rc 保活，销毁时先清除窗口用户数据；后台置顶任务未传递裸 Rust 对象指针。未发现可直接归因的内存生命周期缺陷
- 当前没有应用级操作日志。Release 的 `panic = "abort"` 不能作为 Hang 的原因；增加 panic hook 也不能定位无异常的同步等待

以下路径存在跨进程同步调用，属于需要通过复现或等待链核对的候选，尚未与历史 4 次故障建立对应关系：

1. 普通透明度首次应用或恢复：`WindowController::apply`／恢复逻辑 → `write_snapshot` → `SetWindowLongPtrW(GWL_EXSTYLE)`。目标线程若不响应样式消息，可阻塞调用方
2. 独立容器移动或缩放：`WM_MOVE`／`WM_SIZE` → 目标布局 → 同步 `SetWindowPos`
3. 启用或取消嵌入容器：跨线程 `SetParent` 及后续样式／位置恢复。父子关系还可能关联输入队列，不能仅增加 `SWP_ASYNCWINDOWPOS` 就声称全部阻塞问题得到解决

预先探测目标响应只能拒绝已经挂起的窗口，不能消除探测后挂起的竞态。后续隔离现场没有显示跨进程等待，因而本轮没有围绕这些候选重构窗口托管、恢复和退出事务。

## 隔离复现与定位

- 用户授权后正常退出原实例，再以独立 `APPDATA` 配置启动 v2.1.5 和临时目标窗口。挂起实例为 PID `56712`，主线程 TID `59744`，窗口位于 `(-44, 382)`，外框 `42 × 182` 物理像素，客户区 `40 × 150` DIP，DPI 为 `96`
- 迷你列表选中临时窗口后停止响应，Trans 的 CPU 时间持续增长，临时目标仍正常响应。WCT 仅返回主线程 `Running`，没有跨进程等待链；现场表现为界面线程忙循环
- 第一份转储的真实 unwind 栈为 `DispatchMessageWorker → ToolTipsWndProc → HandleTimer → ShowBubbleForTool → DoShowBubble+0x441 → NtUserSetWindowPos`。寄存器及只读窗口元数据确认该 `SetWindowPos` 操作的目标为同一进程的 `tooltips_class32`
- 第二份转储仍在同一提示展示流程，进入 `DoShowBubble+0x4C9 → GetTipSize → CListView::OnNotify → BeginFakeItemDraw → CIFakeCustomDrawNotify → Trans 自绘`。两次间隔约两分钟的采样与持续 CPU 增长共同支持原生名称提示的测量／定位反馈循环
- 使用本地 DbgEng 实际栈展开，并从微软官方符号服务器取得与转储 GUID／age 匹配的系统符号。优化后的 Trans 私有 Rust 帧缺少可靠名称，输出中的 `_cflush+0` 等伪匹配不作为函数定位证据；栈不能继续说明 Windows 私有布局算法中的具体错误语句
- 完成两份转储后，只结束本次拥有的挂起隔离实例；没有把无响应作为普通模式滚条或外部程序故障处理

## 修复与验证范围

修复仅调整 `src/mini_list.rs` 的扩展样式：设置掩码包含 `LVS_EX_LABELTIP`，实际值不包含它，确保明确关闭原生截断名称浮层。普通列表原本没有主动开启该样式。本轮没有新增替代提示机制，也没有改动非客户区计算、列宽和透明度逻辑；长名称可通过拉宽窗口或展开普通模式查看。

v2.1.6 使用独立配置，在相同 40 DIP／96 DPI／屏幕边缘位置进行有限对照：

- 双击临时目标可进入迷你调节页，并可返回列表；再次选择仍可进入调节页
- 名称悬停观察约 `28.05` 秒，进程持续响应；CPU 时间由 `2.65625` 秒增加至 `2.71875` 秒，未出现基线持续忙转
- 竖向滑条获焦后按上键，界面显示由 `0%` 变为 `1%`
- `fixed-slider-target.json` 使用有符号的 `ctypes.wintypes.BYTE` 解码 alpha，输出 `-4`，其无符号字节值为 `252`；原脚本的 `matches=false` 是比较时未转换符号导致的诊断错误。保留原始文件，不将该布尔值作为功能失败或通过的断言
- 用户按物理 Esc 停止电脑控制后立即停止输入。随后恢复操作的结果未确认，正常宽度、多项目滚轮和更多跨屏场景未完成回归，隔离测试窗口及临时目标尚未清理；不能声称全部功能已验收或用户环境已恢复到测试前状态

修复通过 Rust 格式／语法解析、diff 和独立静态复核；离线锁定依赖 Release 打包成功。未运行 Cargo 单元测试。

交付文件为 `dist/release-20261107-150226/Trans.exe`，版本 `2.1.6`，大小 `724,992` 字节，SHA256 为 `F2C54D6AB2B65477FBE162ADE5DD5E96C6FB598DEC5B9DEA20C159D8FD79B43F`。发布脚本完成源产物与发布文件哈希核对；记录收尾时只读复核发布文件与 sidecar 一致、PE 为 x64 Windows GUI PE32+，FileVersion／ProductVersion 均为 `2.1.6`。

## 证据索引与剩余工作

- 原始事件 XML／JSON：`target/crash-investigation-20261107/application-events.xml`、`application-events.json`
- 当前主线程只读等待链及脚本：`target/crash-investigation-20261107/current-wait-chain.json`、`current-wait-chain.py`
- 基线版本配对：`v2.1.5-Trans.exe`、`v2.1.5-Trans.pdb`
- 挂起现场：`hung-geometry.json`、`mini-hang-wct.json`、`mini-hang.dmp`、`mini-hang-second.dmp`
- 符号化真实调用栈：`mini-hang-symbol-stack.txt`、`mini-hang-second-symbol-stack.txt`、`symbolized-hang-findings.json`、`ms-symbols/download-manifest.json`
- 修复对照：`fixed-hover-before.json`、`fixed-hover-after.json`、`fixed-slider-target.json`、`fixed-process.json`，以及本次会话中的界面截图
- 后续若获准继续电脑控制，应先确认隔离测试进程状态，再完成正常宽度、多项目滚轮、跨屏和清理检查；不要自动结束当前用户使用的实例
- 未提交或推送代码。以上未注明目录的证据文件均在 `target/crash-investigation-20261107/` 下

本目录下的 `target/`、`dist/` 为相对仓库根目录的本地路径，不是远程下载链接。
