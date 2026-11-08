# 2026-11-04 窗口缩放残影与按钮丢失审查

本文记录对应 `release-20261104-172106`。后续根据持续闪动反馈，已调整交互刷新时机、像素保留和设置面板通知去重，最新实现与验证范围见 [开发与验证记录](validation-history.md)。

## 范围与结论

用户确认问题发生在拖动边框调整大小时。本轮重点审查主窗口消息调度、普通／迷你／设置页布局、控件显隐、原生控件批量定位、自绘标题栏、按钮与 GDI 缓冲生命周期，并对当前可编译程序和测试目标执行严格 Clippy 检查。

发现的确定性代码缺口是：画面失效被间接依赖于布局变化；仅提交 `RDW_UPDATENOW` 不能保证已丢失像素的控件被重新绘制。另有补绘发生在嵌套尺寸消息内部，以及边框刷新没有尺寸消息时缺少补绘入口的问题。这些缺口与用户现象相符，但本轮未启动真实窗口，不能将静态证据和自动化结果认定为实机根因已经唯一确定。

## 发现与处理

| 级别 | 问题 | 修复 |
| --- | --- | --- |
| P1 | `layout_window` 仅执行 `RDW_UPDATENOW \| RDW_ALLCHILDREN`，完整失效标记由 `App::layout` 提供；当布局缓存命中时，画面恢复没有完整失效区保证 | 增加 `WindowRedraw`，将画面失效与布局缓存分离；每次正式补绘显式包含客户区、背景、标题栏和所有子控件 |
| P2 | `WM_WINDOWPOSCHANGED` 默认处理同步发送 `WM_SIZE`，旧代码在其嵌套处理中同步补绘，不能合并一轮尺寸提交、字体更新及控件定位 | 尺寸提交和 DPI 更新期间合并请求，完成后、应用状态借用释放后补绘；原生按钮绘制和文本查询回调不触发整窗补绘 |
| P2 | 同尺寸 `SWP_FRAMECHANGED`、`SWP_NOCOPYBITS` 等表面变化没有完整补绘入口，单纯记录窗口位置不能恢复控件 | 处理 `WM_WINDOWPOSCHANGED` 的表面变化，保留默认消息转发；纯移动和单纯 Z 序变化不额外重绘 |
| P2 | 同步绘制的新请求与失败重试缺乏独立状态 | 保存待绘标记、抑制递归；绘制中新增请求交由下一轮事件处理，失败保留请求但不忙循环 |
| 优化 | 每次缩放都重复设置百分比文字的字体和对齐样式 | 仅在字体或样式实际改变时写入，减少布局期间的同步通知 |
| 清理 | 当前目标存在 9 处 Clippy 告警，涉及冗余转换、嵌套条件、指针比较和 Copy 类型方法约定 | 做等价简化，严格检查不再有告警；这些不是已确认的残影根因 |

主窗口保留此前的 `WVR_REDRAW`、`SWP_NOCOPYBITS`、标题栏双缓冲与系统圆角。独立容器的嵌入／兼容模式未改动，相关清理仅简化已有条件表达式。

相关接口依据：[RedrawWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-redrawwindow) 说明 `RDW_UPDATENOW` 处理待绘区域，`RDW_FRAME` 和 `RDW_ERASE` 需要配合 `RDW_INVALIDATE`；[WM_WINDOWPOSCHANGED](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-windowposchanged) 说明默认处理发送 `WM_SIZE`／`WM_MOVE`；[SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos) 说明无重绘定位由应用负责补绘，以及 `SWP_NOCOPYBITS` 会丢弃客户区内容。

## 验证

- Debug 自动化测试 41 项通过，最终源码的 Release 自动化测试 41 项通过
- 新增 6 项调度测试，覆盖缓存命中时恢复按钮、同尺寸表面变化、嵌套更新合并、重入请求保留、失败后重试、空闲和退出时无额外绘制
- 新增 2 项真实内存 GDI 按钮测试：8 种按钮样式 × 4 轮尺寸 × 3 种状态，共 96 组完整像素比较；另验证 4 种图标按钮局部刷新能还原完整图像。测试只使用内存 DC 和 DIB，不创建窗口或读取桌面
- `cargo clippy --offline --locked --target x86_64-pc-windows-msvc --bin Trans --tests -- -D warnings` 通过，格式和差异检查通过
- 隔离副本恢复旧的不完整失效标记后，出现预期的 1 项失败；移除尺寸处理中延后补绘的保护后，出现预期的 2 项失败。正式源码未回退
- 验证日志位于 `target/review-resize-20261104/`：`unit-tests.log`、`release-tests.log`、`clippy.log`、`missing-invalidation.log`、`early-paint.log`

调度测试使用状态模型，像素测试调用真实 GDI 绘制函数。新增按钮测试使用空窗口句柄及空标签，覆盖 96 DPI 回退下的图形、背景和裁剪，不覆盖实际标签、焦点和鼠标交互。上述测试不能替代真实 HWND 消息时序和 DWM 合成验证。按用户限制未启动 Trans、未进行电脑控制、未运行会打开或操作窗口的 GUI 脚本，也未读写用户配置。

交付产物和哈希记录见 [开发与验证记录](validation-history.md)。
