# 2026-10-16 代码审阅

本轮以修改前的工作区为基线，审阅当前参与构建的 Rust 模块、容器恢复与输入流程、原生控件资源、配置存储，以及 Release 脚本。保留此前未提交修改和已知不兼容窗口的容器限制。

## 已修复的问题

### P1：宿主重入销毁后，活动调用仍可能引用已释放的状态

- 位置：[window_container.rs](../src/window_container.rs)
- 原状态由宿主独占的 `Box` 管理，`WM_NCDESTROY` 直接释放。布局、恢复和窗口回调持有状态引用跨越同步 Win32 / COM 调用，嵌套消息若销毁宿主，外层调用仍可能继续访问旧引用
- 改为控制器、宿主和每次活动回调分别持有 `Rc`。销毁回调先清除窗口指针，再释放宿主持有的引用，活动调用结束后才允许状态释放
- 已逐支核对创建失败、安装用户数据失败、正常销毁与重入销毁时的引用平衡；此结论来自源码审阅，尚未进行桌面复现

### P2：任务栏操作可能使用失效或已复用的宿主句柄

- 位置：[window_container.rs](../src/window_container.rs)、[window_control.rs](../src/window_control.rs)
- 原 `WindowContainer::hwnd()` 直接返回缓存 HWND，任务栏操作未核验宿主是否仍为原实例
- 现在返回 `Result`，调用前核对进程、线程、实例标记、状态指针和窗口过程。COM 初始化之后重新取得已核验的句柄；自动维护遇到错误时记录失败并保留恢复对象，手动操作向上返回错误

### P2：宿主消失后，仍存活的原目标可能遗漏恢复

- 位置：[window_container.rs](../src/window_container.rs)
- 原 `detach()` 在宿主不存在时直接返回成功，未处理已自行脱离宿主、仍保留嵌入样式的目标
- 现在使用控制器保留的快照继续核验并恢复目标的父关系、样式和位置；恢复失败保留记录。旧宿主 HWND 已被复用时仍拒绝操作，不把新窗口当作原宿主

### P2：取消关机或注销后，窗口管理状态已被提前清除

- 位置：[app.rs](../src/app.rs)
- 原代码收到 `WM_QUERYENDSESSION` 就恢复全部窗口；其他应用阻止关机或用户随后取消时，Trans 仍继续运行，但管理状态已经清除
- 查询阶段现在立即同意，不修改应用状态；仅在 `WM_ENDSESSION` 确认结束时停止活动事件与计时器、阻断后续普通事件并恢复窗口
- 依据：[WM_QUERYENDSESSION](https://learn.microsoft.com/en-us/windows/win32/shutdown/wm-queryendsession)、[WM_ENDSESSION](https://learn.microsoft.com/en-us/windows/win32/shutdown/wm-endsession)。Windows 将查询与确认结束分开，清理应延后到确认结束

### P3：设置面板定位失败后，完整布局可能被错误缓存

- 位置：[settings_panel.rs](../src/settings_panel.rs)、[app.rs](../src/app.rs)
- 原面板定位失败没有向主布局返回失败，主窗口仍可能缓存当前尺寸，后续相同尺寸的布局请求无法重试
- `set_bounds()` 现在返回实际定位结果，面板和两批子控件全部成功后才缓存完整布局；失败时清除请求尺寸缓存，允许后续重新布局

### P2：发布脚本覆盖固定旧包，与版本目录规则不一致

- 位置：[build-release.ps1](../scripts/build-release.ps1)、[README](../README.md#从源码构建)
- 原脚本覆盖 `dist/Trans.exe`，旧文件正在运行时会发布失败，也不能保留上一版本
- 现在新建 `dist/release-yyyyMMdd-HHmmss/`，拒绝覆盖已有目录或 EXE；校验源文件和发布文件的 x64 GUI PE 格式及 SHA256，再生成校验文件
- 默认离线构建，显式 `-AllowOnline` 才允许 Cargo 联网；每轮只构建一次，并在退出时恢复临时修改的环境变量

## 分配优化

- 窗口枚举排序改为缓存标题小写键，每个窗口只转换一次，保持标题、PID、HWND 的原有排序顺序
- 空搜索直接通过名称条件，避免逐个窗口创建小写标题和 PID 字符串，保留排除自身及已登记窗口的过滤

## 审阅与验证边界

- 独立复核了容器引用计数、宿主身份验证调用、布局失败处理和会话结束分支
- Rust 格式与语法解析、PowerShell AST、接口引用和 diff 静态检查通过
- Release 打包及产物信息记录在[开发与验证记录](validation-history.md)
- 未运行测试套件、启动应用或进行桌面操作；构建和静态审阅不能代替真实第三方窗口、DPI、拖动与异常恢复的运行验证
- `container_input.rs` 当前未被主模块引用，本轮未将其接回，也未重新引入已撤回的实验兼容方案
