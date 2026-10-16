use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::{HWND, PROPERTYKEY, VARIANT_TRUE};
use windows_sys::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows_sys::Win32::System::Variant::{VARENUM, VT_BOOL, VT_EMPTY, VT_LPWSTR};
use windows_sys::Win32::UI::Shell::PropertiesSystem::SHGetPropertyStoreForWindow;
use windows_sys::Win32::UI::Shell::TaskbarList as CLSID_TASKBAR_LIST;

use crate::localization::tr;

const IID_ITASKBAR_LIST: GUID = GUID::from_u128(0x56fdf342_fd6d_11d0_958a_006097c9a090);
const IID_IPROPERTY_STORE: GUID = GUID::from_u128(0x886d8eeb_8cf2_4446_8d02_cdba1dbdcf99);
const APP_USER_MODEL_FORMAT: GUID = GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3);
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_USER_MODEL_FORMAT,
    pid: 5,
};
const PKEY_APP_USER_MODEL_PREVENT_PINNING: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_USER_MODEL_FORMAT,
    pid: 9,
};

// 方法顺序及参数类型对应 Windows SDK 的 IPropertyStore
#[repr(C)]
struct PropertyStoreVTable {
    query_interface: unsafe extern "system" fn(
        *mut PropertyStoreInterface,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut PropertyStoreInterface) -> u32,
    release: unsafe extern "system" fn(*mut PropertyStoreInterface) -> u32,
    get_count: unsafe extern "system" fn(*mut PropertyStoreInterface, *mut u32) -> HRESULT,
    get_at:
        unsafe extern "system" fn(*mut PropertyStoreInterface, u32, *mut PROPERTYKEY) -> HRESULT,
    get_value: unsafe extern "system" fn(
        *mut PropertyStoreInterface,
        *const PROPERTYKEY,
        *mut PROPVARIANT,
    ) -> HRESULT,
    set_value: unsafe extern "system" fn(
        *mut PropertyStoreInterface,
        *const PROPERTYKEY,
        *const PROPVARIANT,
    ) -> HRESULT,
    commit: unsafe extern "system" fn(*mut PropertyStoreInterface) -> HRESULT,
}

#[repr(C)]
struct PropertyStoreInterface {
    vtable: *const PropertyStoreVTable,
}

// windows-sys 不提供 COM 接口封装，布局对应 Windows SDK 的 ITaskbarList
#[repr(C)]
struct TaskbarListVTable {
    query_interface: unsafe extern "system" fn(
        *mut TaskbarListInterface,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut TaskbarListInterface) -> u32,
    release: unsafe extern "system" fn(*mut TaskbarListInterface) -> u32,
    hr_init: unsafe extern "system" fn(*mut TaskbarListInterface) -> HRESULT,
    add_tab: unsafe extern "system" fn(*mut TaskbarListInterface, HWND) -> HRESULT,
    delete_tab: unsafe extern "system" fn(*mut TaskbarListInterface, HWND) -> HRESULT,
    activate_tab: unsafe extern "system" fn(*mut TaskbarListInterface, HWND) -> HRESULT,
    set_active_alt: unsafe extern "system" fn(*mut TaskbarListInterface, HWND) -> HRESULT,
}

#[repr(C)]
struct TaskbarListInterface {
    vtable: *const TaskbarListVTable,
}

struct ComApartment;

impl ComApartment {
    fn new() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        if result < 0 {
            return Err(com_error(
                tr(
                    "无法初始化任务栏 COM 接口",
                    "Cannot initialize the taskbar COM interface",
                ),
                result,
            ));
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

// 仅用于 Trans 新建的容器窗口，首次显示前创建，并在窗口销毁前于同一线程释放
pub(crate) struct ContainerTaskbarGroup {
    interface: *mut PropertyStoreInterface,
    _apartment: ComApartment,
}

impl ContainerTaskbarGroup {
    pub(crate) fn new(hwnd: HWND, target_process_id: u32) -> Result<Self, String> {
        let apartment = ComApartment::new()?;
        let mut interface = null_mut::<c_void>();
        let result =
            unsafe { SHGetPropertyStoreForWindow(hwnd, &IID_IPROPERTY_STORE, &mut interface) };
        if result < 0 {
            if !interface.is_null() {
                let interface = interface.cast::<PropertyStoreInterface>();
                unsafe { ((*(*interface).vtable).release)(interface) };
            }
            return Err(com_error(
                tr(
                    "无法访问容器任务栏属性",
                    "Cannot access the container taskbar properties",
                ),
                result,
            ));
        }
        if interface.is_null() {
            return Err(tr(
                "Windows 未返回容器任务栏属性接口",
                "Windows did not return a container taskbar property interface",
            )
            .into());
        }
        let group = Self {
            interface: interface.cast(),
            _apartment: apartment,
        };

        // 临时宿主没有可重新启动目标软件的命令，必须在分组标识前禁止固定
        let prevent_pinning = property_variant(
            VT_BOOL,
            PROPVARIANT_0_0_0 {
                boolVal: VARIANT_TRUE,
            },
        );
        group.set_value(&PKEY_APP_USER_MODEL_PREVENT_PINNING, &prevent_pinning)?;

        let mut identity: Vec<u16> = format!("Trans.Container.Process{target_process_id}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let identity_value = property_variant(
            VT_LPWSTR,
            PROPVARIANT_0_0_0 {
                pwszVal: identity.as_mut_ptr(),
            },
        );
        // SetValue 复制字符串，变体仅借用 identity 缓冲区，不能调用 PropVariantClear
        group.set_value(&PKEY_APP_USER_MODEL_ID, &identity_value)?;
        Ok(group)
    }

    fn set_value(&self, key: &PROPERTYKEY, value: &PROPVARIANT) -> Result<(), String> {
        let result = unsafe { ((*(*self.interface).vtable).set_value)(self.interface, key, value) };
        if result < 0 {
            Err(com_error(
                tr(
                    "无法设置容器任务栏分组",
                    "Cannot configure the container taskbar group",
                ),
                result,
            ))
        } else {
            // 窗口属性存储即时生效，不需要调用 Commit
            Ok(())
        }
    }
}

impl Drop for ContainerTaskbarGroup {
    fn drop(&mut self) {
        // 成功释放及初始化中途失败均清空属性，让窗口属性存储释放字符串副本
        let empty = property_variant(VT_EMPTY, PROPVARIANT_0_0_0::default());
        let _ = self.set_value(&PKEY_APP_USER_MODEL_ID, &empty);
        let _ = self.set_value(&PKEY_APP_USER_MODEL_PREVENT_PINNING, &empty);
        unsafe { ((*(*self.interface).vtable).release)(self.interface) };
    }
}

fn property_variant(vt: VARENUM, value: PROPVARIANT_0_0_0) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: PROPVARIANT_0_0 {
                vt,
                Anonymous: value,
                ..PROPVARIANT_0_0::default()
            },
        },
    }
}

pub(crate) struct TaskbarList {
    interface: *mut TaskbarListInterface,
    _apartment: ComApartment,
}

impl TaskbarList {
    pub(crate) fn new() -> Result<Self, String> {
        let apartment = ComApartment::new()?;
        let mut interface = null_mut::<c_void>();
        let result = unsafe {
            CoCreateInstance(
                &CLSID_TASKBAR_LIST,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ITASKBAR_LIST,
                &mut interface,
            )
        };
        if result < 0 {
            return Err(com_error(
                tr(
                    "无法连接 Windows 任务栏",
                    "Cannot connect to the Windows taskbar",
                ),
                result,
            ));
        }
        if interface.is_null() {
            return Err(tr(
                "Windows 未返回任务栏接口",
                "Windows did not return a taskbar interface",
            )
            .into());
        }
        let taskbar = Self {
            interface: interface.cast(),
            _apartment: apartment,
        };
        let result = unsafe { ((*(*taskbar.interface).vtable).hr_init)(taskbar.interface) };
        if result < 0 {
            return Err(com_error(
                tr(
                    "无法初始化 Windows 任务栏",
                    "Cannot initialize the Windows taskbar",
                ),
                result,
            ));
        }
        Ok(taskbar)
    }

    pub(crate) fn set_hidden(&self, hwnd: HWND, hidden: bool) -> Result<(), String> {
        let result = unsafe {
            let vtable = &*(*self.interface).vtable;
            if hidden {
                (vtable.delete_tab)(self.interface, hwnd)
            } else {
                (vtable.add_tab)(self.interface, hwnd)
            }
        };
        if result < 0 {
            Err(com_error(
                if hidden {
                    tr("无法隐藏任务栏按钮", "Cannot hide the taskbar button")
                } else {
                    tr("无法恢复任务栏按钮", "Cannot restore the taskbar button")
                },
                result,
            ))
        } else {
            Ok(())
        }
    }
}

impl Drop for TaskbarList {
    fn drop(&mut self) {
        unsafe { ((*(*self.interface).vtable).release)(self.interface) };
    }
}

fn com_error(action: &str, result: HRESULT) -> String {
    let mut message = format!("{action}{}HRESULT 0x{:08X}", tr("，", "; "), result as u32);
    if matches!(result as u32, 0x80070005 | 0x80070522) {
        message.push_str(tr(
            "。目标可能以更高权限运行，可尝试以管理员身份启动 Trans",
            ". The target may run with elevated privileges; try running Trans as administrator",
        ));
    }
    message
}
