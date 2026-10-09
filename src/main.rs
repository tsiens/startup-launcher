#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::ImageEncoder;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    mem::size_of,
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use windows_sys::Win32::UI::{
    Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON},
    WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM},
    Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS,
    },
    System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_SZ,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetParent, GetWindowTextLengthW, GetWindowTextW,
        IsWindowVisible, PostMessageW, WM_CLOSE,
    },
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "Startup Launcher";
const UPDATE_REPOSITORY: &str = "tsiens/startup-launcher";
const NO_LAUNCH_JUMP: usize = usize::MAX;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct LaunchEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default, rename = "iconPath")]
    icon_path: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(default)]
    windows: Vec<String>,
    #[serde(default)]
    delay: u32,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Config {
    #[serde(
        default = "default_network_target",
        rename = "networkTarget",
        alias = "network_target"
    )]
    network_target: String,
    #[serde(default)]
    entries: Vec<LaunchEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            network_target: default_network_target(),
            entries: Vec::new(),
        }
    }
}

fn default_network_target() -> String {
    "223.5.5.5".into()
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupState {
    config: Config,
    registered: bool,
    autostart: bool,
    version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupResult {
    registered: bool,
    message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchProgress {
    index: usize,
    status: String,
    phase: String,
    remaining_seconds: Option<u32>,
    window_title: Option<String>,
    window_index: Option<usize>,
    window_count: Option<usize>,
    started: bool,
}

struct WindowStageInfo<'a> {
    title: &'a str,
    index: usize,
    count: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateNotice {
    version: String,
    current_version: &'static str,
}

#[derive(Clone)]
struct AvailableUpdate {
    version: String,
    url: String,
}

#[derive(Default)]
struct PendingUpdate(Arc<Mutex<Option<AvailableUpdate>>>);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchRequest {
    config: Config,
    check_after: bool,
}

#[derive(Default)]
struct PendingLaunch(Mutex<Option<LaunchRequest>>);

#[derive(Clone, Default)]
struct LaunchControl {
    jump_to: Arc<AtomicUsize>,
    network_ready: Arc<AtomicBool>,
    entry_count: Arc<AtomicUsize>,
    started_entries: Arc<Mutex<HashSet<usize>>>,
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn config_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("StartupLauncher")
        .join("startup.json")
}

fn load_config(path: &Path) -> Result<Config, String> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let bytes = fs::read(path).map_err(|e| format!("读取配置失败：{e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("配置文件格式错误：{e}"))
}

fn save_config(config: &Config) -> Result<(), String> {
    let path = config_path();
    let parent = path.parent().ok_or_else(|| "配置目录无效".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败：{e}"))?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| format!("序列化配置失败：{e}"))?;
    fs::write(path, bytes).map_err(|e| format!("保存配置失败：{e}"))
}

fn registry_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取程序路径失败：{e}"))?;
    Ok(format!("\"{}\" --autostart", exe.display()))
}

fn registry_open(access: u32) -> Result<HKEY, String> {
    let key_name = wide(RUN_KEY);
    let mut key = std::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            access | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if status == 0 {
        Ok(key)
    } else {
        Err(format!(
            "打开自启动注册表失败：{}",
            std::io::Error::from_raw_os_error(status as i32)
        ))
    }
}

fn startup_registered() -> Result<bool, String> {
    let key = match registry_open(KEY_QUERY_VALUE) {
        Ok(key) => key,
        Err(_) => return Ok(false),
    };
    let name = wide(RUN_VALUE);
    let mut size = 0u32;
    let mut kind = 0u32;
    let first = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if first != 0 {
        unsafe { RegCloseKey(key) };
        return Ok(false);
    }
    let mut data = vec![0u16; (size as usize).div_ceil(size_of::<u16>())];
    let second = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    if second != 0 || kind != REG_SZ {
        return Ok(false);
    }
    let len = data.iter().position(|c| *c == 0).unwrap_or(data.len());
    Ok(String::from_utf16_lossy(&data[..len]) == registry_command()?)
}

fn set_startup_registered(enabled: bool) -> Result<(), String> {
    let key_name = wide(RUN_KEY);
    let mut key = std::ptr::null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if result != 0 {
        return Err(format!(
            "打开自启动注册表失败：{}",
            std::io::Error::from_raw_os_error(result as i32)
        ));
    }
    let name = wide(RUN_VALUE);
    let status = if enabled {
        let data = wide(&registry_command()?);
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * size_of::<u16>()) as u32,
            )
        }
    } else {
        unsafe { RegDeleteValueW(key, name.as_ptr()) }
    };
    unsafe { RegCloseKey(key) };
    if status == 0 || (!enabled && status == 2) {
        Ok(())
    } else {
        Err(format!(
            "修改自启动失败：{}",
            std::io::Error::from_raw_os_error(status as i32)
        ))
    }
}

fn is_autostart() -> bool {
    std::env::args().skip(1).any(|arg| arg == "--autostart")
}

#[tauri::command]
fn get_state() -> Result<StartupState, String> {
    Ok(StartupState {
        config: load_config(&config_path())?,
        registered: startup_registered().unwrap_or(false),
        autostart: is_autostart(),
        version: env!("STARTUP_LAUNCHER_VERSION").into(),
    })
}

#[tauri::command]
fn set_autostart(config: Config, enabled: bool) -> Result<StartupResult, String> {
    if enabled {
        save_config(&config)?;
        set_startup_registered(true)?;
        Ok(StartupResult {
            registered: true,
            message: "自启动配置已保存".into(),
        })
    } else {
        set_startup_registered(false)?;
        Ok(StartupResult {
            registered: false,
            message: "已取消开机自启动".into(),
        })
    }
}

#[tauri::command]
fn browse_executable() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Windows programs", &["exe"])
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
fn executable_icon(path: String) -> Option<String> {
    let path_w = wide(&path);
    let mut file_info = SHFILEINFOW::default();
    let result = unsafe {
        SHGetFileInfoW(
            path_w.as_ptr(),
            0,
            &mut file_info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if result == 0 || file_info.hIcon.is_null() {
        return None;
    }

    let mut icon_info = ICONINFO::default();
    let icon_ok = unsafe { GetIconInfo(file_info.hIcon, &mut icon_info) } != 0;
    let mut png = None;
    if icon_ok && !icon_info.hbmColor.is_null() {
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if !dc.is_null() {
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 32,
                    biHeight: -32,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut rgba = vec![0u8; 32 * 32 * 4];
            let lines = unsafe {
                GetDIBits(
                    dc,
                    icon_info.hbmColor,
                    0,
                    32,
                    rgba.as_mut_ptr().cast(),
                    &mut info,
                    DIB_RGB_COLORS,
                )
            };
            unsafe {
                DeleteDC(dc);
                DeleteObject(icon_info.hbmColor);
                if !icon_info.hbmMask.is_null() {
                    DeleteObject(icon_info.hbmMask);
                }
                DestroyIcon(file_info.hIcon);
            }
            if lines == 32 {
                let has_alpha = rgba.chunks_exact(4).any(|pixel| pixel[3] != 0);
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                    if !has_alpha {
                        pixel[3] = 255;
                    }
                }
                let mut bytes = Vec::new();
                if image::codecs::png::PngEncoder::new(&mut bytes)
                    .write_image(&rgba, 32, 32, image::ExtendedColorType::Rgba8)
                    .is_ok()
                {
                    png = Some(format!("data:image/png;base64,{}", BASE64.encode(bytes)));
                }
            }
        } else {
            unsafe {
                DeleteObject(icon_info.hbmColor);
                if !icon_info.hbmMask.is_null() {
                    DeleteObject(icon_info.hbmMask);
                }
                DestroyIcon(file_info.hIcon);
            }
        }
    } else {
        unsafe {
            if icon_ok && !icon_info.hbmMask.is_null() {
                DeleteObject(icon_info.hbmMask);
            }
            DestroyIcon(file_info.hIcon);
        }
    }
    png
}

#[tauri::command]
fn default_icon() -> String {
    format!(
        "data:image/x-icon;base64,{}",
        BASE64.encode(include_bytes!("../assets/app.ico"))
    )
}

#[tauri::command]
fn run_sequence(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    control: State<'_, LaunchControl>,
    mut config: Config,
    check_after: bool,
) {
    retain_enabled_entries(&mut config);
    let pending = pending.0.clone();
    control.jump_to.store(NO_LAUNCH_JUMP, Ordering::SeqCst);
    control.network_ready.store(false, Ordering::SeqCst);
    control
        .entry_count
        .store(config.entries.len(), Ordering::SeqCst);
    if let Ok(mut started) = control.started_entries.lock() {
        started.clear();
    }
    let control = control.inner().clone();
    thread::spawn(move || run_sequence_worker(app, pending, control, config, check_after));
}

#[tauri::command]
fn set_main_window_visible(app: AppHandle, visible: bool) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;
    if visible {
        window.show().map_err(|e| format!("显示主窗口失败：{e}"))?;
        window
            .set_focus()
            .map_err(|e| format!("聚焦主窗口失败：{e}"))?;
    } else {
        window.hide().map_err(|e| format!("隐藏主窗口失败：{e}"))?;
    }
    Ok(())
}

#[tauri::command]
fn close_main_window(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn open_launch_progress(
    app: AppHandle,
    pending: State<'_, PendingLaunch>,
    mut config: Config,
    check_after: bool,
) -> Result<(), String> {
    retain_enabled_entries(&mut config);
    if let Some(window) = app.get_webview_window("launch-progress") {
        window
            .set_focus()
            .map_err(|e| format!("聚焦启动进度窗口失败：{e}"))?;
        return Ok(());
    }

    *pending.0.lock().map_err(|_| "启动进度请求不可用")? = Some(LaunchRequest {
        config,
        check_after,
    });

    let window = match WebviewWindowBuilder::new(
        &app,
        "launch-progress",
        WebviewUrl::App("progress.html".into()),
    )
    .title("Startup Launcher")
    .background_color(tauri::utils::config::Color(239, 245, 253, 255))
    .inner_size(400.0, 225.0)
    .min_inner_size(360.0, 220.0)
    .center()
    .decorations(false)
    .build()
    {
        Ok(window) => window,
        Err(error) => {
            if let Ok(mut request) = pending.0.lock() {
                request.take();
            }
            return Err(format!("创建启动进度窗口失败：{error}"));
        }
    };

    let app_for_close = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::CloseRequested { .. }) {
            app_for_close.exit(0);
        }
    });
    Ok(())
}

#[tauri::command]
fn take_launch_request(pending: State<'_, PendingLaunch>) -> Option<LaunchRequest> {
    pending.0.lock().ok()?.take()
}

#[tauri::command]
fn close_launch_progress(app: AppHandle) -> Result<(), String> {
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn dismiss_launch_progress(app: AppHandle) -> Result<(), String> {
    if let Some(progress) = app.get_webview_window("launch-progress") {
        progress
            .destroy()
            .map_err(|e| format!("关闭启动进度窗口失败：{e}"))?;
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.set_focus();
    }
    Ok(())
}

#[tauri::command]
fn jump_to_launch(index: usize, control: State<'_, LaunchControl>) -> Result<(), String> {
    if !control.network_ready.load(Ordering::SeqCst) {
        return Err("联网检测通过前不能跳过".into());
    }
    if index >= control.entry_count.load(Ordering::SeqCst) {
        return Err("目标启动项无效".into());
    }
    if control
        .started_entries
        .lock()
        .map_err(|_| "启动状态不可用")?
        .contains(&index)
    {
        return Err("该启动项已经启动".into());
    }
    control.jump_to.store(index, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn check_updates(app: AppHandle, pending: State<'_, PendingUpdate>) {
    check_updates_worker(app, pending.0.clone());
}

#[tauri::command]
fn install_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .map_err(|_| "更新状态不可用")?
        .take()
        .ok_or_else(|| "没有待安装的更新".to_string())?;
    let _ = app.emit(
        "update-status",
        format!("正在下载 {}，完成后将替换程序并重启…", update.version),
    );
    thread::spawn(move || {
        let version = update.version.clone();
        match download_update(&update) {
            Ok(()) => {
                let _ = app.emit(
                    "update-status",
                    format!("已下载 {version}，正在替换并重启…"),
                );
                thread::sleep(Duration::from_millis(350));
                app.exit(0);
            }
            Err(error) => {
                let _ = app.emit("update-error", format!("更新失败：{error}"));
            }
        }
    });
    Ok(())
}

fn run_sequence_worker(
    app: AppHandle,
    pending: Arc<Mutex<Option<AvailableUpdate>>>,
    control: LaunchControl,
    config: Config,
    check_after: bool,
) {
    let target = config.network_target.trim();
    if !target.is_empty() {
        loop {
            let _ = app.emit("network-progress", format!("正在检测网络连接：{target}"));
            if network_target_reachable(target) {
                break;
            }
            thread::sleep(Duration::from_secs(3));
        }
    }
    control.network_ready.store(true, Ordering::SeqCst);
    let _ = app.emit("network-ready", ());

    let mut index = 0;
    'entries: while index < config.entries.len() {
        if let Some(target) = take_launch_jump(&control) {
            index = target;
        }
        if control
            .started_entries
            .lock()
            .is_ok_and(|started| started.contains(&index))
        {
            index += 1;
            continue;
        }
        let entry = &config.entries[index];
        if entry.path.trim().is_empty() {
            emit_launch(&app, index, "跳过：路径为空", "launch", None);
            index += 1;
            continue;
        }
        let mut command = Command::new(entry.path.trim());
        command.args(entry.args.iter().filter(|arg| !arg.is_empty()));
        match command.spawn() {
            Ok(_child) => {
                if let Ok(mut started) = control.started_entries.lock() {
                    started.insert(index);
                }
                let stages = window_detection_stages(entry);
                if stages.is_empty() {
                    if let Some(delay) = entry_delay_seconds(entry) {
                        for remaining in (1..=delay).rev() {
                            emit_launch(
                                &app,
                                index,
                                "已启动，等待下一项",
                                "launch-delay",
                                Some(remaining),
                            );
                            for _ in 0..10 {
                                if let Some(target) = take_launch_jump(&control) {
                                    index = target;
                                    continue 'entries;
                                }
                                thread::sleep(Duration::from_millis(100));
                            }
                        }
                    } else {
                        emit_launch(&app, index, "已启动", "launch", None);
                    }
                } else {
                    let mut seen_windows = HashSet::new();
                    let mut last_window = None;
                    let mut all_windows_found = true;
                    for (stage_index, window) in stages.iter().enumerate() {
                        if let Some(target) = take_launch_jump(&control) {
                            index = target;
                            continue 'entries;
                        }
                        let title = window.trim();
                        let mut detected = None;
                        for remaining in (1..=60).rev() {
                            if let Some(target) = take_launch_jump(&control) {
                                index = target;
                                continue 'entries;
                            }
                            emit_window_progress(
                                &app,
                                index,
                                WindowStageInfo {
                                    title,
                                    index: stage_index + 1,
                                    count: stages.len(),
                                },
                                "window-wait",
                                Some(remaining),
                                &format!("等待窗口：{title}"),
                            );
                            detected = matching_window(title, &seen_windows);
                            if detected.is_some() {
                                break;
                            }
                            for _ in 0..10 {
                                if let Some(target) = take_launch_jump(&control) {
                                    index = target;
                                    continue 'entries;
                                }
                                thread::sleep(Duration::from_millis(100));
                            }
                        }
                        if let Some(target) = take_launch_jump(&control) {
                            index = target;
                            continue 'entries;
                        }
                        if let Some(hwnd) = detected {
                            seen_windows.insert(hwnd as isize);
                            last_window = Some(hwnd);
                            emit_window_progress(
                                &app,
                                index,
                                WindowStageInfo {
                                    title,
                                    index: stage_index + 1,
                                    count: stages.len(),
                                },
                                "window-found",
                                None,
                                &format!("窗口已出现：{title}"),
                            );
                        } else {
                            all_windows_found = false;
                            emit_window_progress(
                                &app,
                                index,
                                WindowStageInfo {
                                    title,
                                    index: stage_index + 1,
                                    count: stages.len(),
                                },
                                "window-timeout",
                                None,
                                &format!("窗口等待超时：{title}"),
                            );
                        }
                    }
                    if all_windows_found {
                        if let (Some(hwnd), Some(delay)) = (last_window, window_close_delay(entry))
                        {
                            let final_stage = stages.last().expect("stages are non-empty");
                            let title = final_stage.trim();
                            for remaining in (1..=delay).rev() {
                                emit_window_progress(
                                    &app,
                                    index,
                                    WindowStageInfo {
                                        title,
                                        index: stages.len(),
                                        count: stages.len(),
                                    },
                                    "window-close-delay",
                                    Some(remaining),
                                    &format!("即将关闭窗口：{title}"),
                                );
                                for _ in 0..10 {
                                    if let Some(target) = take_launch_jump(&control) {
                                        index = target;
                                        continue 'entries;
                                    }
                                    thread::sleep(Duration::from_millis(100));
                                }
                            }
                            let closed = unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } != 0;
                            let status = if closed {
                                format!("已请求关闭窗口：{title}")
                            } else {
                                format!("关闭窗口失败：{title}")
                            };
                            emit_window_progress(
                                &app,
                                index,
                                WindowStageInfo {
                                    title,
                                    index: stages.len(),
                                    count: stages.len(),
                                },
                                "window-close",
                                None,
                                &status,
                            );
                        }
                    }
                }
            }
            Err(error) => emit_launch(&app, index, &format!("启动失败：{error}"), "launch", None),
        }
        index += 1;
    }
    control.network_ready.store(false, Ordering::SeqCst);
    let _ = app.emit("launch-finished", check_after);
    if check_after {
        check_updates_worker(app, pending);
    }
}

fn window_close_delay(entry: &LaunchEntry) -> Option<u32> {
    (!window_detection_stages(entry).is_empty())
        .then(|| entry_delay_seconds(entry))
        .flatten()
}

fn entry_delay_seconds(entry: &LaunchEntry) -> Option<u32> {
    (entry.delay > 0).then_some(entry.delay.min(86_400))
}

fn retain_enabled_entries(config: &mut Config) {
    config.entries.retain(|entry| entry.enabled);
}

fn window_detection_stages(entry: &LaunchEntry) -> Vec<&str> {
    entry
        .windows
        .iter()
        .map(String::as_str)
        .filter(|window| !window.trim().is_empty())
        .collect()
}

fn network_target_reachable(target: &str) -> bool {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Command::new("curl.exe")
            .args([
                "--silent",
                "--show-error",
                "--location",
                "--max-time",
                "5",
                "--output",
                "NUL",
                target,
            ])
            .creation_flags(0x08000000)
            .status()
            .is_ok_and(|status| status.success());
    }
    let host = target.trim_matches(['[', ']']);
    if let Ok(ip) = host.parse::<IpAddr>() {
        if TcpStream::connect_timeout(&SocketAddr::new(ip, 80), Duration::from_secs(2)).is_ok() {
            return true;
        }
    } else {
        let address = format!("{host}:80");
        if address.to_socket_addrs().is_ok_and(|addresses| {
            addresses
                .into_iter()
                .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok())
        }) {
            return true;
        }
    }
    Command::new("ping.exe")
        .args(["-n", "1", "-w", "2000", host])
        .creation_flags(0x08000000)
        .status()
        .is_ok_and(|status| status.success())
}

fn emit_launch(
    app: &AppHandle,
    index: usize,
    status: &str,
    phase: &str,
    remaining_seconds: Option<u32>,
) {
    let started = status.starts_with("已启动")
        || status.starts_with("窗口已出现：")
        || status.starts_with("窗口等待超时：");
    let _ = app.emit(
        "launch-progress",
        LaunchProgress {
            index,
            status: status.into(),
            phase: phase.into(),
            remaining_seconds,
            window_title: None,
            window_index: None,
            window_count: None,
            started,
        },
    );
}

fn emit_window_progress(
    app: &AppHandle,
    index: usize,
    window: WindowStageInfo<'_>,
    phase: &str,
    remaining_seconds: Option<u32>,
    status: &str,
) {
    let _ = app.emit(
        "launch-progress",
        LaunchProgress {
            index,
            status: status.into(),
            phase: phase.into(),
            remaining_seconds,
            window_title: Some(window.title.into()),
            window_index: Some(window.index),
            window_count: Some(window.count),
            started: true,
        },
    );
}

fn take_launch_jump(control: &LaunchControl) -> Option<usize> {
    let target = control.jump_to.swap(NO_LAUNCH_JUMP, Ordering::SeqCst);
    (target != NO_LAUNCH_JUMP).then_some(target)
}

fn matching_window(title: &str, seen: &HashSet<isize>) -> Option<HWND> {
    struct Search<'a> {
        title: String,
        seen: &'a HashSet<isize>,
        matches: Vec<HWND>,
    }
    unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> i32 {
        let search = &mut *(param as *mut Search<'_>);
        if IsWindowVisible(hwnd) == 0
            || !GetParent(hwnd).is_null()
            || search.seen.contains(&(hwnd as isize))
            || GetWindowTextLengthW(hwnd) == 0
        {
            return 1;
        }
        let mut buffer = vec![0u16; 1024];
        let len = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32).max(0) as usize;
        let got = String::from_utf16_lossy(&buffer[..len])
            .trim()
            .to_lowercase();
        if got == search.title {
            search.matches.push(hwnd);
        }
        1
    }
    let mut search = Search {
        title: title.trim().to_lowercase(),
        seen,
        matches: Vec::new(),
    };
    unsafe {
        EnumWindows(Some(collect), &mut search as *mut _ as LPARAM);
    }
    let foreground = unsafe { GetForegroundWindow() };
    search
        .matches
        .into_iter()
        .min_by_key(|hwnd| *hwnd != foreground)
}

fn check_updates_worker(app: AppHandle, pending: Arc<Mutex<Option<AvailableUpdate>>>) {
    thread::spawn(move || match check_for_update() {
        Ok(Some(update)) => {
            if let Ok(mut current) = pending.lock() {
                *current = Some(update.clone());
            }
            let _ = app.emit(
                "update-available",
                UpdateNotice {
                    version: update.version,
                    current_version: env!("STARTUP_LAUNCHER_VERSION"),
                },
            );
        }
        Ok(None) => {
            let _ = app.emit(
                "update-current",
                format!("已是最新版本（{}）", env!("STARTUP_LAUNCHER_VERSION")),
            );
        }
        Err(error) => {
            let message = if error.contains("代理也无法连接") {
                "更新检查失败：GitHub 暂时无法连接，启动项仍可正常使用".to_string()
            } else {
                format!("更新检查失败：{error}")
            };
            let _ = app.emit("update-error", message);
        }
    });
}

fn version_is_newer(candidate: &str) -> Result<bool, String> {
    fn parse(value: &str) -> Option<[u64; 3]> {
        let value = value.strip_prefix('v').unwrap_or(value);
        let mut parts = value.split('.');
        let version = [
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ];
        parts.next().is_none().then_some(version)
    }
    let candidate = parse(candidate).ok_or_else(|| format!("Release tag 格式无效：{candidate}"))?;
    let current = parse(env!("STARTUP_LAUNCHER_VERSION")).expect("package version is valid");
    Ok(candidate > current)
}

fn run_curl(url: &str, output: Option<&Path>) -> Result<Vec<u8>, String> {
    let attempt = |request_url: &str| -> Result<Vec<u8>, String> {
        let mut command = Command::new("curl.exe");
        command.args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            "5",
            "--max-time",
            "60",
        ]);
        if let Some(path) = output {
            command.arg("--output").arg(path);
        }
        let result = command
            .arg(request_url)
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| format!("启动系统 curl.exe 失败：{e}"))?;
        if !result.status.success() {
            return Err(String::from_utf8_lossy(&result.stderr).trim().to_string());
        }
        Ok(result.stdout)
    };
    match attempt(url) {
        Ok(bytes) => Ok(bytes),
        Err(direct_error) => {
            if let Some(path) = output {
                let _ = fs::remove_file(path);
            }
            let proxy_url = format!("https://gh-proxy.org/{url}");
            attempt(&proxy_url).map_err(|proxy_error| {
                format!("GitHub 直连失败（{direct_error}）；代理也无法连接（{proxy_error}）")
            })
        }
    }
}

fn check_for_update() -> Result<Option<AvailableUpdate>, String> {
    let api_url = format!("https://api.github.com/repos/{UPDATE_REPOSITORY}/releases/latest");
    let release_bytes = run_curl(&api_url, None)?;
    let release: serde_json::Value = serde_json::from_slice(&release_bytes)
        .map_err(|e| format!("解析 Release 信息失败：{e}"))?;
    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("Release 缺少 tag_name")?;
    if !version_is_newer(tag)? {
        return Ok(None);
    }
    let assets = release
        .get("assets")
        .and_then(|v| v.as_array())
        .ok_or("Release 没有附件列表")?;
    let asset = assets
        .iter()
        .find(|asset| asset.get("name").and_then(|v| v.as_str()) == Some("StartupLauncher.exe"))
        .ok_or("最新 Release 中没有 StartupLauncher.exe")?;
    let url = asset
        .get("browser_download_url")
        .and_then(|v| v.as_str())
        .ok_or("更新附件缺少下载地址")?;
    Ok(Some(AvailableUpdate {
        version: tag.to_string(),
        url: url.to_string(),
    }))
}

fn download_update(update: &AvailableUpdate) -> Result<(), String> {
    let temp = std::env::temp_dir().join(format!(
        "StartupLauncher-download-{}.exe",
        std::process::id()
    ));
    run_curl(&update.url, Some(&temp))?;
    let bytes = fs::read(&temp).map_err(|e| format!("读取更新文件失败：{e}"))?;
    let _ = fs::remove_file(&temp);
    prepare_self_update(&bytes)
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn prepare_self_update(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 32_768 || bytes.get(..2) != Some(b"MZ") {
        return Err("下载文件不是有效的 Windows 程序".into());
    }
    let current = std::env::current_exe().map_err(|e| format!("获取当前程序路径失败：{e}"))?;
    let temp =
        std::env::temp_dir().join(format!("StartupLauncher-update-{}.exe", std::process::id()));
    let script =
        std::env::temp_dir().join(format!("StartupLauncher-update-{}.ps1", std::process::id()));
    fs::write(&temp, bytes).map_err(|e| format!("保存更新文件失败：{e}"))?;
    let script_text = format!(
        "$ErrorActionPreference = 'Stop'\n$source = {}\n$target = {}\n$targetProcessId = {}\n$deadline = (Get-Date).AddSeconds(45)\nwhile ((Get-Process -Id $targetProcessId -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {{ Start-Sleep -Milliseconds 200 }}\nfor ($attempt = 0; $attempt -lt 30; $attempt++) {{ try {{ Copy-Item -LiteralPath $source -Destination $target -Force; Start-Process -FilePath $target; Remove-Item -LiteralPath $source -Force -ErrorAction SilentlyContinue; Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue; exit 0 }} catch {{ Start-Sleep -Milliseconds 300 }} }}\n",
        ps_quote(&temp.to_string_lossy()), ps_quote(&current.to_string_lossy()), std::process::id()
    );
    fs::write(&script, script_text).map_err(|e| format!("创建更新脚本失败：{e}"))?;
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("启动更新程序失败：{e}"))?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(PendingUpdate::default())
        .manage(PendingLaunch::default())
        .manage(LaunchControl::default())
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_autostart,
            set_main_window_visible,
            close_main_window,
            browse_executable,
            executable_icon,
            default_icon,
            run_sequence,
            open_launch_progress,
            take_launch_request,
            close_launch_progress,
            dismiss_launch_progress,
            jump_to_launch,
            check_updates,
            install_update
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Startup Launcher");
}

#[cfg(test)]
mod tests {
    use super::{
        entry_delay_seconds, load_config, retain_enabled_entries, window_close_delay,
        window_detection_stages, Config, LaunchEntry,
    };
    use std::{env, time::SystemTime};

    #[test]
    fn missing_config_starts_with_no_entries() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!("startup-launcher-{nonce}.json"));
        assert!(load_config(&path).unwrap().entries.is_empty());
    }

    #[test]
    fn network_target_defaults_and_round_trips_in_camel_case() {
        let config: Config = serde_json::from_str(r#"{"entries":[]}"#).unwrap();
        assert_eq!(config.network_target, "223.5.5.5");
        let saved = serde_json::to_value(config).unwrap();
        assert_eq!(saved["networkTarget"], "223.5.5.5");
        assert!(saved.get("network_target").is_none());
    }

    #[test]
    fn existing_entries_default_to_enabled() {
        let entry: LaunchEntry = serde_json::from_str(r#"{"name":"Existing"}"#).unwrap();
        assert!(entry.enabled);
    }

    #[test]
    fn icon_path_is_optional_and_uses_camel_case() {
        let entry: LaunchEntry =
            serde_json::from_str(r#"{"iconPath":"D:/Apps/Icon.exe"}"#).unwrap();
        assert_eq!(entry.icon_path, "D:/Apps/Icon.exe");
        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(value["iconPath"], "D:/Apps/Icon.exe");
        assert!(value.get("icon_path").is_none());
    }

    #[test]
    fn disabled_entries_are_removed_from_the_launch_sequence() {
        let mut config = Config {
            network_target: String::new(),
            entries: vec![
                LaunchEntry {
                    name: "Enabled".into(),
                    enabled: true,
                    ..LaunchEntry::default()
                },
                LaunchEntry {
                    name: "Disabled".into(),
                    enabled: false,
                    ..LaunchEntry::default()
                },
            ],
        };
        retain_enabled_entries(&mut config);
        assert_eq!(config.entries.len(), 1);
        assert_eq!(config.entries[0].name, "Enabled");
    }

    #[test]
    fn delay_is_stored_once_after_the_window_titles() {
        let entry: LaunchEntry =
            serde_json::from_str(r#"{"windows":["DingTalk","DingTalk"],"delay":1}"#).unwrap();
        assert_eq!(entry.windows.len(), 2);
        assert_eq!(entry.windows[0], "DingTalk");
        assert_eq!(entry.windows[1], "DingTalk");
        assert_eq!(window_close_delay(&entry), Some(1));
        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(value["delay"], 1);
        assert!(value.get("close_delay_seconds").is_none());
        assert_eq!(
            value["windows"],
            serde_json::json!(["DingTalk", "DingTalk"])
        );
    }

    #[test]
    fn window_titles_require_the_string_array_format() {
        assert!(
            serde_json::from_str::<LaunchEntry>(r#"{"windows":[{"title":"DingTalk"}]}"#).is_err()
        );
    }

    #[test]
    fn close_delay_requires_window_titles_and_is_capped() {
        let no_windows = LaunchEntry {
            delay: 10,
            ..LaunchEntry::default()
        };
        assert_eq!(entry_delay_seconds(&no_windows), Some(10));
        assert_eq!(window_close_delay(&no_windows), None);
        assert_eq!(
            window_close_delay(&LaunchEntry {
                windows: vec!["Main".into()],
                delay: 100_000,
                ..LaunchEntry::default()
            }),
            Some(86_400)
        );
    }

    #[test]
    fn window_detection_stages_preserve_title_order_and_duplicates() {
        let entry = LaunchEntry {
            windows: vec!["Main".into(), "  ".into(), "Main".into()],
            ..LaunchEntry::default()
        };
        let stages = window_detection_stages(&entry);
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0], "Main");
        assert_eq!(stages[1], "Main");
    }
}
