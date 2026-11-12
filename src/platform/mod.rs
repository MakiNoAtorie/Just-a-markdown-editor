use cosmic_text::{FontSystem, fontdb};
use std::path::{Path, PathBuf};
use winit::keyboard::ModifiersState;

#[cfg(all(test, windows))]
pub fn test_event_loop<T: 'static>() -> winit::event_loop::EventLoop<T> {
    use winit::platform::windows::EventLoopBuilderExtWindows;
    winit::event_loop::EventLoop::<T>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap()
}

pub fn primary(modifiers: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    }
}
pub fn font_system() -> FontSystem {
    let mut db = fontdb::Database::new();
    #[cfg(windows)]
    for name in [
        "segoeui.ttf",
        "segoeuib.ttf",
        "segoeuii.ttf",
        "seguisym.ttf",
        "consola.ttf",
    ] {
        let folder = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        let _ = db.load_font_file(folder.join("Fonts").join(name));
    }
    #[cfg(target_os = "macos")]
    for path in [
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Menlo.ttc",
    ] {
        let _ = db.load_font_file(path);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    db.load_system_fonts();
    db.set_sans_serif_family(if cfg!(windows) {
        "Segoe UI"
    } else {
        "Helvetica"
    });
    db.set_monospace_family(if cfg!(windows) { "Consolas" } else { "Menlo" });
    FontSystem::new_with_locale_and_db("en-US".into(), db)
}
pub fn fallback_fonts() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    db.set_sans_serif_family(if cfg!(windows) {
        "Segoe UI"
    } else {
        "Helvetica"
    });
    db.set_monospace_family(if cfg!(windows) { "Consolas" } else { "Menlo" });
    db
}
pub fn workspace_file(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let absolute = path.canonicalize().ok()?;
    Some((
        absolute.parent()?.to_path_buf(),
        PathBuf::from(absolute.file_name()?),
    ))
}

pub fn save_destination(path: &Path) -> Option<PathBuf> {
    Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?))
}

/// Modern Explorer-style folder picker (IFileOpenDialog).
#[cfg(windows)]
pub fn choose_folder(owner: usize) -> Option<PathBuf> {
    use std::{ffi::OsString, ffi::c_void, os::windows::ffi::OsStringExt};
    use windows_sys::{
        Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
        core::GUID,
    };
    type This = *mut c_void;
    // Slots in the COM vtables: IUnknown is 0-2, then IModalWindow::Show, then IFileDialog.
    const RELEASE: usize = 2;
    const SHOW: usize = 3;
    const SET_OPTIONS: usize = 9;
    const GET_OPTIONS: usize = 10;
    const SET_TITLE: usize = 17;
    const GET_RESULT: usize = 20;
    const ITEM_GET_DISPLAY_NAME: usize = 5;
    const FOS_PICKFOLDERS: u32 = 0x20;
    const FOS_FORCEFILESYSTEM: u32 = 0x40;
    const SIGDN_FILESYSPATH: i32 = 0x8005_8000_u32 as i32;
    unsafe fn slot<T: Copy>(object: This, index: usize) -> T {
        unsafe {
            let table = *(object as *const *const usize);
            std::mem::transmute_copy(&*table.add(index))
        }
    }
    unsafe {
        let initialized = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) >= 0;
        let clsid = GUID::from_u128(0xDC1C5A9C_E88A_4DDE_A5A1_60F82A20AEF7);
        let iid = GUID::from_u128(0xD57C7288_D4AD_4768_BE02_9D969532D960);
        let mut dialog: This = std::ptr::null_mut();
        let created = CoCreateInstance(&clsid, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &iid, &mut dialog);
        let mut result = None;
        if created >= 0 && !dialog.is_null() {
            let release: unsafe extern "system" fn(This) -> u32 = slot(dialog, RELEASE);
            let get_options: unsafe extern "system" fn(This, *mut u32) -> i32 = slot(dialog, GET_OPTIONS);
            let set_options: unsafe extern "system" fn(This, u32) -> i32 = slot(dialog, SET_OPTIONS);
            let set_title: unsafe extern "system" fn(This, *const u16) -> i32 = slot(dialog, SET_TITLE);
            let show: unsafe extern "system" fn(This, This) -> i32 = slot(dialog, SHOW);
            let get_result: unsafe extern "system" fn(This, *mut This) -> i32 = slot(dialog, GET_RESULT);
            let mut options = 0u32;
            get_options(dialog, &mut options);
            set_options(dialog, options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
            let title: Vec<u16> = "Choose your Markdown workspace\0".encode_utf16().collect();
            set_title(dialog, title.as_ptr());
            if show(dialog, owner as This) >= 0 {
                let mut item: This = std::ptr::null_mut();
                if get_result(dialog, &mut item) >= 0 && !item.is_null() {
                    let name: unsafe extern "system" fn(This, i32, *mut *mut u16) -> i32 =
                        slot(item, ITEM_GET_DISPLAY_NAME);
                    let item_release: unsafe extern "system" fn(This) -> u32 = slot(item, RELEASE);
                    let mut text: *mut u16 = std::ptr::null_mut();
                    if name(item, SIGDN_FILESYSPATH, &mut text) >= 0 && !text.is_null() {
                        let mut length = 0;
                        while *text.add(length) != 0 {
                            length += 1;
                        }
                        result = Some(PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(text, length))));
                        CoTaskMemFree(text.cast());
                    }
                    item_release(item);
                }
            }
            release(dialog);
        }
        if initialized {
            CoUninitialize();
        }
        result
    }
}
#[cfg(target_os = "macos")]
pub fn choose_folder(_: usize) -> Option<PathBuf> {
    let output = std::process::Command::new("osascript")
        .args(["-e", "POSIX path of (choose folder)"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(PathBuf::from(String::from_utf8(output.stdout).ok()?.trim()))
    } else {
        None
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn choose_folder(_: usize) -> Option<PathBuf> {
    None
}

/// Moves a file or folder to the Recycle Bin without UI.
#[cfg(windows)]
pub fn recycle(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::*;
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    // SHFileOperationW does not accept verbatim (\\?\) paths.
    let mut from = if wide.starts_with(&prefix(r"\\?\UNC\")) {
        [prefix(r"\\"), wide[8..].to_vec()].concat()
    } else if wide.starts_with(&prefix(r"\\?\")) {
        wide[4..].to_vec()
    } else {
        wide
    };
    // pFrom is a list that ends with two NULs.
    from.extend([0, 0]);
    let mut operation: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    operation.wFunc = FO_DELETE;
    operation.pFrom = from.as_ptr();
    operation.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
    let code = unsafe { SHFileOperationW(&mut operation) };
    if code != 0 || operation.fAnyOperationsAborted != 0 {
        return Err(std::io::Error::other(format!("Could not move to the Recycle Bin (code {code})")));
    }
    Ok(())
}
#[cfg(not(windows))]
pub fn recycle(_: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "No Recycle Bin on this platform"))
}

pub fn data_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[derive(Default)]
pub struct Clipboard(Option<arboard::Clipboard>);
impl Clipboard {
    fn get(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.0.is_none() {
            self.0 = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        Ok(self.0.as_mut().unwrap())
    }
    pub fn read(&mut self) -> Result<String, String> {
        self.get()?.get_text().map_err(|e| e.to_string())
    }
    pub fn write(&mut self, text: String) -> Result<(), String> {
        self.get()?.set_text(text).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
pub fn choose_file(save: bool, owner: usize) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::Controls::Dialogs::*;
    let mut name = vec![0u16; 32768];
    if save {
        for (index, c) in "Untitled.md".encode_utf16().enumerate() {
            name[index] = c;
        }
    }
    let filter: Vec<u16> = "Markdown (*.md;*.markdown)\0*.md;*.markdown\0All files\0*.*\0\0"
        .encode_utf16()
        .collect();
    let extension: Vec<u16> = "md\0".encode_utf16().collect();
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = owner as _;
    dialog.lpstrFile = name.as_mut_ptr();
    dialog.nMaxFile = name.len() as u32;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrDefExt = extension.as_ptr();
    dialog.Flags = OFN_EXPLORER
        | OFN_NOCHANGEDIR
        | OFN_PATHMUSTEXIST
        | if save {
            OFN_OVERWRITEPROMPT
        } else {
            OFN_FILEMUSTEXIST
        };
    let selected = unsafe {
        if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    };
    if selected == 0 {
        None
    } else {
        let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        Some(PathBuf::from(std::ffi::OsString::from_wide(&name[..end])))
    }
}
#[cfg(target_os = "macos")]
pub fn choose_file(save: bool, _: usize) -> Option<PathBuf> {
    let script = if save {
        "POSIX path of (choose file name default name \"Untitled.md\")"
    } else {
        "POSIX path of (choose file)"
    };
    let result = std::process::Command::new("osascript")
        .args(["-e", script])
        .output()
        .ok()?;
    if result.status.success() {
        Some(PathBuf::from(String::from_utf8(result.stdout).ok()?.trim()))
    } else {
        None
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn choose_file(_: bool, _: usize) -> Option<PathBuf> {
    None
}

/// The icon that build.rs embeds as resource 1.
pub fn app_icon() -> Option<winit::window::Icon> {
    #[cfg(windows)]
    {
        use winit::platform::windows::IconExtWindows;
        return winit::window::Icon::from_resource(1, None).ok();
    }
    #[allow(unreachable_code)]
    None
}

pub fn window_owner(window: &winit::window::Window) -> usize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get() as usize,
        _ => 0,
    }
}

/// Opens a web or mail link in the default app. Other schemes are refused.
pub fn open_url(url: &str) -> bool {
    let allowed = ["http://", "https://", "mailto:"].iter().any(|s| url.to_lowercase().starts_with(s));
    if !allowed || url.chars().any(char::is_control) {
        return false;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (verb, target) = (wide("open"), wide(url));
        // Values above 32 mean success.
        return unsafe {
            ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), 1) as usize > 32
        };
    }
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open").arg(url).status().is_ok_and(|s| s.success());
    }
    #[allow(unreachable_code)]
    false
}
