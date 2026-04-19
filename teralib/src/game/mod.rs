// Cross-platform imports
use crate::config;
#[cfg(windows)]
use lazy_static::lazy_static;
use log::{error, info, Level, Metadata, Record};
#[cfg(not(windows))]
use log::warn;
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    process::ExitStatus,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc as other_mpsc, watch};

// Windows-only imports
#[cfg(windows)]
use crate::global_credentials::{set_credentials, GLOBAL_CREDENTIALS};
#[cfg(windows)]
use prost::Message;
#[cfg(windows)]
use reqwest;
#[cfg(windows)]
use serde_json::Value;
#[cfg(windows)]
use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
    process::Command,
    ptr::null_mut,
    slice,
    sync::{mpsc, Arc, Mutex, RwLock},
    time::Duration,
};
#[cfg(windows)]
use tokio::{runtime::Runtime, sync::Notify};
#[cfg(windows)]
use winapi::{
    shared::{
        minwindef::{BOOL, LPARAM, LRESULT, TRUE, UINT, WPARAM},
        windef::HWND,
    },
    um::{
        errhandlingapi::GetLastError,
        libloaderapi::GetModuleHandleW,
        winuser::{GetClassInfoExW, *},
    },
};

// Windows-only constants
#[cfg(windows)]
const WM_GAME_EXITED: u32 = WM_USER + 1;

/// Exit information recorded from the last LauncherGameExitNotification (event 1020).
#[derive(Debug, Clone, Default)]
pub struct GameExitInfo {
    pub code:   u32,
    pub reason: u32,
}

/// Details string from the last LauncherGameCrashNotification (event 1021).
/// Stored as a plain UTF-8 string after decoding the UTF-16 payload.
#[cfg(windows)]
static LAST_EXIT_INFO: Lazy<Mutex<GameExitInfo>> =
    Lazy::new(|| Mutex::new(GameExitInfo::default()));

#[cfg(windows)]
static LAST_CRASH_DETAILS: Lazy<Mutex<String>> =
    Lazy::new(|| Mutex::new(String::new()));

/// Stderr captured from the TERA.exe process during the last game session.
#[cfg(windows)]
static LAST_GAME_STDERR: Lazy<Mutex<String>> =
    Lazy::new(|| Mutex::new(String::new()));

/// Returns a copy of the exit info recorded from the last game session.
/// Code and reason are both 0 if the game has not exited yet or exited normally.
pub fn get_last_exit_info() -> GameExitInfo {
    #[cfg(windows)]
    {
        LAST_EXIT_INFO.lock().unwrap().clone()
    }
    #[cfg(not(windows))]
    {
        GameExitInfo::default()
    }
}

/// Returns crash details from the last game session (empty if no crash).
pub fn get_last_crash_details() -> String {
    #[cfg(windows)]
    {
        LAST_CRASH_DETAILS.lock().unwrap().clone()
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

/// Returns the stderr output captured from TERA.exe during the last game session.
/// Empty if nothing was written to stderr.
pub fn get_last_game_stderr() -> String {
    #[cfg(windows)]
    {
        LAST_GAME_STDERR.lock().unwrap().clone()
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

/// Module for handling server list functionality.
///
/// This module includes the generated code from the `_serverlist_proto.rs` file,
/// which likely contains protobuf-generated structures and functions for
/// managing server list data.
#[cfg(windows)]
mod serverlist {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "\\src\\_serverlist_proto.rs"
    ));
}
#[cfg(windows)]
use serverlist::{server_list::ServerInfo, ServerList};

// Windows-only global static variables
#[cfg(windows)]
lazy_static! {
    static ref SERVER_LIST_SENDER: Mutex<Option<mpsc::Sender<(WPARAM, usize)>>> = Mutex::new(None);

    static ref ACTS_MAP: RwLock<HashMap<String, String>> = RwLock::new(HashMap::new());
    static ref PAGES_MAP: RwLock<HashMap<String, String>> = RwLock::new(HashMap::new());
}

/// Handle to the game window (Windows-only).
#[cfg(windows)]
static WINDOW_HANDLE: Lazy<Mutex<Option<SafeHWND>>> = Lazy::new(|| Mutex::new(None));

/// Flag indicating whether the game is currently running.
///
/// This atomic boolean is used to track the running state of the game
/// across multiple threads.
static GAME_RUNNING: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

/// Sender for game status updates.
///
/// This channel sender is used to broadcast changes in the game's running state
/// to any interested receivers.
static GAME_STATUS_SENDER: Lazy<watch::Sender<bool>> = Lazy::new(|| {
    let (tx, _) = watch::channel(false);
    tx
});

// Windows-only struct definitions
#[cfg(windows)]
#[derive(Clone, Copy)]
struct SafeHWND(HWND);

// Implementations
#[cfg(windows)]
unsafe impl Send for SafeHWND {}
#[cfg(windows)]
unsafe impl Sync for SafeHWND {}

#[cfg(windows)]
impl SafeHWND {
    /// Creates a new `SafeHWND` instance.
    ///
    /// This function wraps a raw `HWND` into a `SafeHWND` struct, providing a safer interface
    /// for handling window handles.
    ///
    /// # Arguments
    ///
    /// * `hwnd` - A raw window handle of type `HWND`.
    ///
    /// # Returns
    ///
    /// A new `SafeHWND` instance containing the provided window handle.
    fn new(hwnd: HWND) -> Self {
        SafeHWND(hwnd)
    }

    /// Retrieves the raw window handle.
    ///
    /// This method provides access to the underlying `HWND` stored in the `SafeHWND` instance.
    ///
    /// # Returns
    ///
    /// The raw `HWND` window handle.
    fn get(&self) -> HWND {
        self.0
    }
}

/// A custom logger for the Tera application.
///
/// This struct implements the `log::Log` trait and provides a way to send log messages
/// through a channel, allowing for asynchronous logging.
pub struct TeraLogger {
    /// The sender half of a channel for log messages.
    sender: other_mpsc::Sender<String>,
}

impl log::Log for TeraLogger {
    /// Checks if a log message with the given metadata should be recorded.
    ///
    /// This method filters log messages based on the target and log level.
    ///
    /// # Arguments
    ///
    /// * `metadata` - The metadata associated with the log record.
    ///
    /// # Returns
    ///
    /// `true` if the log message should be recorded, `false` otherwise.
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.target().starts_with("teralib") && metadata.level() <= Level::Info
    }

    /// Records a log message.
    ///
    /// If the log message is enabled based on its metadata, this method formats the message
    /// and sends it through the channel.
    ///
    /// # Arguments
    ///
    /// * `record` - The log record to be processed.
    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let log_message = format!("{} - {}", record.level(), record.args());
            let _ = self.sender.try_send(log_message);
        }
    }

    /// Flushes any buffered records.
    ///
    /// This implementation does nothing as there is no buffering.
    fn flush(&self) {}
}

/// Sets up logging for the application.
///
/// This function initializes the global logger with an Info level filter.
/// It uses a lazy initialization pattern to ensure the logger is only set up once.
pub fn setup_logging() -> (TeraLogger, other_mpsc::Receiver<String>) {
    let (sender, receiver) = other_mpsc::channel(100);
    (TeraLogger { sender }, receiver)
}

/// Windows implementation: uses Win32 IPC to communicate with Tera.exe.
#[cfg(windows)]
pub async fn run_game(
    account_name: &str,
    characters_count: &str,
    ticket: &str,
    game_lang: &str,
    game_path: &str,
    acts_map: HashMap<String, String>,
    pages_map: HashMap<String, String>,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    info!("Starting run_game function");

    if is_game_running() {
        return Err("Game is already running".into());
    }

    {
        let mut acts_map_guard = ACTS_MAP.write().unwrap();
        *acts_map_guard = acts_map;
        info!("actsMap received with {} entries", acts_map_guard.len());
    }

    {
        let mut pages_map_guard = PAGES_MAP.write().unwrap();
        *pages_map_guard = pages_map;
        info!("pagesMap received with {} entries", pages_map_guard.len());
    }

    set_credentials(account_name, characters_count, ticket, game_lang, game_path);

    if cfg!(debug_assertions) {
        info!(
            "Set credentials - Account: {}, Characters_count: {}, Ticket: {}, Lang: {}, Game Path: {}",
            GLOBAL_CREDENTIALS.get_account_name(),
            GLOBAL_CREDENTIALS.get_characters_count(),
            GLOBAL_CREDENTIALS.get_ticket(),
            GLOBAL_CREDENTIALS.get_game_lang(),
            GLOBAL_CREDENTIALS.get_game_path()
        );
    } else {
        info!(
            "Set credentials - Characters_count: {}, Lang: {}, Game Path: {}",
            GLOBAL_CREDENTIALS.get_characters_count(),
            GLOBAL_CREDENTIALS.get_game_lang(),
            GLOBAL_CREDENTIALS.get_game_path()
        );
    }

    launch_game().await
}

/// Linux implementation: delegates Win32 IPC to launcher-bridge.exe running under Wine.
/// The native launcher communicates with the bridge via stdin/stdout pipes.
#[cfg(not(windows))]
pub async fn run_game(
    account_name: &str,
    characters_count: &str,
    ticket: &str,
    game_lang: &str,
    game_path: &str,
    acts_map: HashMap<String, String>,
    pages_map: HashMap<String, String>,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        process::Command,
    };

    if is_game_running() {
        return Err("Game is already running".into());
    }

    GAME_RUNNING.store(true, Ordering::SeqCst);
    let _ = GAME_STATUS_SENDER.send(true);

    // Find launcher-bridge.exe next to the native launcher binary
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().ok_or("No parent directory for launcher executable")?;
    let bridge_path = exe_dir.join("launcher-bridge.exe");

    if !bridge_path.exists() {
        GAME_RUNNING.store(false, Ordering::SeqCst);
        let _ = GAME_STATUS_SENDER.send(false);
        return Err(format!(
            "launcher-bridge.exe not found at {:?}. Place it next to the launcher binary.",
            bridge_path
        ).into());
    }

    // Convert Linux absolute path to Wine Z: path (Wine maps / to Z:\)
    let wine_game_path = if game_path.starts_with('/') {
        format!("Z:{}", game_path.replace('/', "\\"))
    } else {
        game_path.to_string()
    };

    // Get server list URL from embedded config
    let server_list_url = config::get_config_value("SERVER_LIST_URL");

    // Serialize credentials for the bridge
    let credentials = serde_json::json!({
        "account_name": account_name,
        "characters_count": characters_count,
        "ticket": ticket,
        "game_lang": game_lang,
        "game_path": wine_game_path,
        "server_list_url": server_list_url,
        "acts_map": acts_map,
        "pages_map": pages_map,
    });

    // Use WINE env var, or prefer wine64 (launcher-bridge.exe is a 64-bit PE),
    // falling back to plain "wine" if wine64 is not on PATH.
    let wine_bin = std::env::var("WINE").unwrap_or_else(|_| {
        if std::process::Command::new("wine64")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            "wine64".to_string()
        } else {
            "wine".to_string()
        }
    });
    info!("Spawning launcher-bridge.exe via '{}'", wine_bin);
    info!("Wine game path: {}", wine_game_path);

    // Resolve WINEPREFIX: must be an absolute path.
    // If the env var is absent or not absolute, fall back to ~/tera-wine.
    let wine_prefix = std::env::var("WINEPREFIX")
        .ok()
        .filter(|p| std::path::Path::new(p).is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{}/tera-wine", home)
        });

    // launcher-bridge.exe is a 64-bit Windows binary; force wine to use a
    // win64 prefix.  Allow the caller to override via WINEARCH if needed.
    let wine_arch = std::env::var("WINEARCH").unwrap_or_else(|_| "win64".to_string());

    info!("Using WINEPREFIX: {}", wine_prefix);
    info!("Using WINEARCH: {}", wine_arch);

    // Ensure the Wine prefix is properly initialised AND is a win64 prefix.
    // A win64 prefix contains drive_c/windows/syswow64/; a win32 prefix does
    // not.  Running wine64 against a win32 prefix causes STATUS_DLL_NOT_FOUND
    // (c0000135) for kernel32.dll.  If we detect a win32 prefix we remove it
    // and recreate it as win64 — it contains no user data at this stage.
    let prefix_path = std::path::Path::new(&wine_prefix);
    let syswow64   = prefix_path.join("drive_c/windows/syswow64");
    let kernel32   = prefix_path.join("drive_c/windows/system32/kernel32.dll");

    let needs_init = if kernel32.exists() && !syswow64.exists() {
        // Prefix exists but is win32 — wipe it and start fresh as win64
        warn!(
            "Wine prefix at {:?} is a win32 prefix but wine64 needs win64 — removing and recreating …",
            prefix_path
        );
        if let Err(e) = std::fs::remove_dir_all(prefix_path) {
            return Err(format!(
                "Failed to remove stale win32 Wine prefix at {:?}: {}. \
                 Please delete it manually and re-launch.",
                prefix_path, e
            ).into());
        }
        true
    } else {
        !kernel32.exists()
    };

    if needs_init {
        info!("Initialising Wine prefix as win64 at {:?} …", prefix_path);
        let display = std::env::var("DISPLAY").unwrap_or_default();

        let boot_status = std::process::Command::new(&wine_bin)
            .args(["wineboot", "--init"])
            .env("WINEPREFIX", &wine_prefix)
            .env("WINEARCH", &wine_arch)
            .env("DISPLAY", &display)
            .status();
        match boot_status {
            Ok(s) if s.success() => info!("wineboot --init completed successfully"),
            Ok(s) => warn!("wineboot --init exited with status {}", s),
            Err(e) => warn!("wineboot --init failed to spawn: {}", e),
        }

        // Wait for wineserver to settle before installing components
        let _ = std::process::Command::new("wineserver")
            .args(["-w"])
            .env("WINEPREFIX", &wine_prefix)
            .status();

        // Install runtime components required by Tera.exe.
        // winetricks is optional — skip gracefully if not installed.
        let has_winetricks = std::process::Command::new("winetricks")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if has_winetricks {
            for component in &["vcrun2013", "vcrun2019", "d3dx9"] {
                info!("Installing Wine component via winetricks: {} …", component);
                let st = std::process::Command::new("winetricks")
                    .args(["-q", component])
                    .env("WINEPREFIX", &wine_prefix)
                    .env("WINEARCH", &wine_arch)
                    .env("DISPLAY", &display)
                    .status();
                match st {
                    Ok(s) if s.success() => info!("winetricks {} installed", component),
                    Ok(s) => warn!("winetricks {} exited with status {} (may already be present)", component, s),
                    Err(e) => warn!("winetricks {} failed to spawn: {}", component, e),
                }
            }
        } else {
            warn!(
                "winetricks not found — skipping vcrun2013/vcrun2019/d3dx9 install. \
                 Tera.exe may fail to start. Run `install-linux-deps.sh` to install it."
            );
        }
    }

    let mut child = Command::new(&wine_bin)
        .arg(&bridge_path)
        .env("WINEPREFIX", &wine_prefix)
        .env("WINEARCH", &wine_arch)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn wine launcher-bridge.exe: {}", e))?;

    // Write JSON credentials to bridge stdin asynchronously, then close it
    {
        let mut stdin = child.stdin.take().ok_or("Failed to get bridge stdin")?;
        let json_bytes = serde_json::to_vec(&credentials)?;
        stdin.write_all(&json_bytes).await?;
        // stdin closes when dropped
    }

    // Read JSON event lines from bridge stdout asynchronously
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line_str)) = lines.next_line().await {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line_str) {
                if let Some(event_type) = event["event"].as_str() {
                    match event_type {
                        "open_website" => {
                            if let Some(url) = event["url"].as_str() {
                                info!("Opening website: {}", url);
                                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                            }
                        }
                        _ => info!("Bridge event: {}", line_str),
                    }
                }
            }
        }
    }

    let status = child.wait().await?;
    info!("Game bridge exited with status: {:?}", status);

    GAME_RUNNING.store(false, Ordering::SeqCst);
    let _ = GAME_STATUS_SENDER.send(false);

    Ok(status)
}

/// Windows-only: Launches the game and handles the game process lifecycle.
#[cfg(windows)]
async fn launch_game() -> Result<ExitStatus, Box<dyn std::error::Error>> {
    if GAME_RUNNING.load(Ordering::SeqCst) {
        return Err("Game is already running".into());
    }

    GAME_RUNNING.store(true, Ordering::SeqCst);
    GAME_STATUS_SENDER.send(true).unwrap();
    info!("Game status set to running");

    if cfg!(debug_assertions) {
        info!(
            "Launching game for account: {}",
            GLOBAL_CREDENTIALS.get_account_name()
        );
    } else {
        info!("Launching game for account");
    }

    let (tx, rx) = mpsc::channel::<(WPARAM, usize)>();
    *SERVER_LIST_SENDER.lock().unwrap() = Some(tx);

    let tcs = Arc::new(tokio::sync::Notify::new());
    let tcs_clone = Arc::clone(&tcs);

    let handle =
        tokio::task::spawn_blocking(move || unsafe { create_and_run_game_window(tcs_clone) });

    tokio::spawn(async move {
        while let Ok((w_param, sender)) = rx.recv() {
            unsafe {
                handle_server_list_request(w_param, sender);
            }
        }
    });

    tcs.notified().await;

    // If window creation failed, WINDOW_HANDLE will still be None.
    // Abort cleanly rather than spawning TERA.exe without an IPC window.
    {
        let handle_guard = WINDOW_HANDLE.lock().unwrap();
        if handle_guard.is_none() {
            GAME_RUNNING.store(false, Ordering::SeqCst);
            let _ = GAME_STATUS_SENDER.send(false);
            return Err("Failed to create launcher IPC window — TERA.exe was not started. \
                        Check logs for the Win32 error code.".into());
        }
    }

    // Clear previous stderr before each launch
    if let Ok(mut s) = LAST_GAME_STDERR.lock() { s.clear(); }

    let mut child = Command::new(GLOBAL_CREDENTIALS.get_game_path())
        .arg(format!(
            "-LANGUAGEEXT={}",
            GLOBAL_CREDENTIALS.get_game_lang()
        ))
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let pid = child.id();
    info!("Game process spawned with PID: {}", pid);

    // Read TERA.exe stderr in a background thread so it doesn't block child.wait().
    // TERA writes human-readable crash info (CrashAddress=, ExceptionCode=, etc.) to stderr.
    let stderr_thread = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut output = String::new();
            let _ = std::io::BufReader::new(stderr).read_to_string(&mut output);
            output
        })
    });

    let status = child.wait()?;
    info!("Game process exited with status: {:?}", status);

    let stderr_output = stderr_thread
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    if !stderr_output.is_empty() {
        info!("Captured {} bytes from TERA.exe stderr", stderr_output.len());
        if let Ok(mut s) = LAST_GAME_STDERR.lock() {
            *s = stderr_output;
        }
    }

    GAME_RUNNING.store(false, Ordering::SeqCst);
    GAME_STATUS_SENDER.send(false).unwrap();
    info!("Game status set to not running");

    if let Ok(handle) = WINDOW_HANDLE.lock() {
        if let Some(safe_hwnd) = *handle {
            let hwnd = safe_hwnd.get();
            unsafe {
                PostMessageW(hwnd, WM_GAME_EXITED, 0, 0);
            }
        } else {
            error!("Window handle not found when trying to post WM_GAME_EXITED message");
        }
    } else {
        error!("Failed to acquire lock on WINDOW_HANDLE");
    }
    handle.await?;

    Ok(status)
}

/// Converts a Rust string slice to a null-terminated wide string (UTF-16).
///
/// This function is useful for interoperability with Windows API functions
/// that expect wide string parameters.
///
/// # Arguments
///
/// * `s` - The input string slice to convert.
///
/// # Returns
///
/// A vector of u16 values representing the wide string, including a null terminator.
#[cfg(windows)]
fn to_wstring(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// Returns a receiver for game status updates.
///
/// This function provides a way to subscribe to game status changes.
///
/// # Returns
///
/// A `watch::Receiver<bool>` that can be used to receive game status updates.
pub fn get_game_status_receiver() -> watch::Receiver<bool> {
    GAME_STATUS_SENDER.subscribe()
}

/// Checks if the game is currently running.
///
/// # Returns
///
/// A boolean indicating whether the game is running (true) or not (false).
pub fn is_game_running() -> bool {
    GAME_RUNNING.load(Ordering::SeqCst)
}

/// Resets the global state of the application.
///
/// This function performs the following actions:
/// 1. Sets the game running status to false.
/// 2. Sends a game status update.
/// 3. Clears the stored window handle.
///
/// It's typically called when cleaning up or restarting the application state.
pub fn reset_global_state() {
    GAME_RUNNING.store(false, Ordering::SeqCst);
    if let Err(e) = GAME_STATUS_SENDER.send(false) {
        error!("Failed to send game status: {:?}", e);
    }
    #[cfg(windows)]
    {
        if let Ok(mut handle) = WINDOW_HANDLE.lock() {
            *handle = None;
        }
        if let Ok(mut map) = ACTS_MAP.write() {
            map.clear();
        }
        if let Ok(mut map) = PAGES_MAP.write() {
            map.clear();
        }
    }
    info!("Global state reset completed");
}

/// Window procedure for handling Windows messages.
///
/// This function is called by the Windows operating system to process messages
/// for the application's window.
///
/// # Safety
///
/// This function is unsafe because it deals directly with raw pointers and
/// Windows API calls.
///
/// # Arguments
///
/// * `h_wnd` - The handle to the window.
/// * `msg` - The message identifier.
/// * `w_param` - Additional message information (depends on the message).
/// * `l_param` - Additional message information (depends on the message).
///
/// # Returns
///
/// The result of the message processing.
#[cfg(windows)]
unsafe extern "system" fn wnd_proc(
    h_wnd: HWND,
    msg: UINT,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    info!("Received message: {}", msg);
    match msg {
        WM_COPYDATA => {
            let copy_data = &*(l_param as *const COPYDATASTRUCT);
            info!("Received WM_COPYDATA message");
            let event_id = copy_data.dwData;
            info!("Event ID: {}", event_id);
            let payload = if copy_data.cbData > 0 {
                slice::from_raw_parts(copy_data.lpData as *const u8, copy_data.cbData as usize)
            } else {
                &[]
            };
            let hex_payload: Vec<String> = payload.iter().map(|b| format!("{:02X}", b)).collect();
            info!("Payload (hex): {}", hex_payload.join(" "));

            match event_id {
                1 => handle_account_name_request(w_param, h_wnd),
                3 => handle_session_ticket_request(w_param, h_wnd),
                5 => handle_server_list_request(w_param, h_wnd as usize),
                7 => handle_enter_lobby_or_world(w_param, h_wnd, payload),
                25 => handle_open_website_command(w_param, h_wnd, payload),
                ///////  TODO
                //26 => handle_web_url_request(w_param, h_wnd, payload), //LauncherWebURLRequest uint32_t id; u16string arguments;
                //27 => handle_web_url_response(w_param, h_wnd, payload), //LauncherWebURLResponse uint32_t id; u16string url;
                ///////
                1000 => handle_game_start(w_param, h_wnd, payload),
                1001..=1016 => handle_game_event(w_param, h_wnd, event_id, payload),
                1020 => handle_game_exit(w_param, h_wnd, payload),
                1021 => handle_game_crash(w_param, h_wnd, payload),
                _ => {
                    info!("Unhandled event ID: {}", event_id);
                }
            }
            1
        }
        WM_GAME_EXITED => {
            info!("Received WM_GAME_EXITED in wnd_proc");
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(h_wnd, msg, w_param, l_param),
    }
}

/// Creates and runs the game window.
///
/// This function sets up the window class, creates the window, and enters
/// the message loop for processing window messages. It also handles cleanup
/// when the window is closed.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers and Windows API calls.
///
/// # Arguments
///
/// * `tcs` - An `Arc<Notify>` used to signal when the window has been created.
#[cfg(windows)]
unsafe fn create_and_run_game_window(tcs: Arc<Notify>) {
    let launcher_class_name = "LAUNCHER_CLASS";
    let launcher_window_title = "LAUNCHER_WINDOW";
    let class_name = to_wstring(launcher_class_name);
    let window_name = to_wstring(launcher_window_title);

    // Unregister any stale class left over from a previous (force-closed) session
    // before attempting to register a fresh one.  Ignore the return value — if the
    // class doesn't exist yet the call is a harmless no-op.
    UnregisterClassW(class_name.as_ptr(), GetModuleHandleW(null_mut()));

    let wnd_class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: GetModuleHandleW(null_mut()),
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null_mut(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: null_mut(),
    };

    let atom = RegisterClassExW(&wnd_class);
    if atom == 0 {
        let err = GetLastError();
        error!("Failed to register window class (error {})", err);
        tcs.notify_one(); // unblock launch_game so it can return an error
        return;
    }

    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        window_name.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        null_mut(),
        null_mut(),
        GetModuleHandleW(null_mut()),
        null_mut(),
    );

    if hwnd.is_null() {
        let err = GetLastError();
        error!("Failed to create window (error {})", err);
        UnregisterClassW(class_name.as_ptr(), GetModuleHandleW(null_mut()));
        tcs.notify_one(); // unblock launch_game so it can return an error
        return;
    }

    info!("Window created with HWND: {:?}", hwnd);

    if let Ok(mut handle) = WINDOW_HANDLE.lock() {
        handle.replace(SafeHWND::new(hwnd));
    } else {
        error!("Failed to acquire lock on WINDOW_HANDLE");
    }

    tcs.notify_one();

    let mut msg = std::mem::zeroed();
    info!("Entering message loop");
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        if msg.message == WM_GAME_EXITED {
            info!("Received WM_GAME_EXITED message");
            break;
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    info!("Exiting message loop");

    DestroyWindow(hwnd);
    UnregisterClassW(class_name.as_ptr(), GetModuleHandleW(null_mut()));

    reset_global_state();

    let mut wcex: WNDCLASSEXW = std::mem::zeroed();
    wcex.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;

    EnumWindows(Some(enum_window_proc), class_name.as_ptr() as LPARAM);

    if GetClassInfoExW(GetModuleHandleW(null_mut()), class_name.as_ptr(), &mut wcex) != 0 {
        if UnregisterClassW(class_name.as_ptr(), GetModuleHandleW(null_mut())) == 0 {
            let error = GetLastError();
            error!("Failed to unregister class. Error code: {}", error);
        } else {
            info!("Tera ClassName Unregistered successfully");
        }
    } else {
        info!("Tera ClassName does not exist or is already unregistered");
    }
}

/// Callback function for enumerating windows.
///
/// This function is called for each top-level window on the screen.
/// It checks if the window's class name matches the given class name,
/// and if so, destroys the window.
///
/// # Safety
///
/// This function is unsafe because it deals with raw window handles and
/// destroys windows, which can have system-wide effects.
///
/// # Arguments
///
/// * `hwnd` - Handle to a top-level window.
/// * `lparam` - Application-defined value given in EnumWindows.
///
/// # Returns
///
/// Returns TRUE to continue enumeration, FALSE to stop.
#[cfg(windows)]
unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let mut class_name: [u16; 256] = [0; 256];
    let len = GetClassNameW(hwnd, class_name.as_mut_ptr(), 256) as usize;
    let class_name = &class_name[..len];

    let search_class = slice::from_raw_parts(lparam as *const u16, 256);
    let search_len = search_class.iter().position(|&c| c == 0).unwrap_or(256);
    let search_class = &search_class[..search_len];

    if class_name.starts_with(search_class) {
        DestroyWindow(hwnd);
    }
    TRUE
}

/// Sends a response message to a specified recipient.
///
/// This function constructs a COPYDATASTRUCT and sends it using the SendMessageW Windows API function.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers and Windows API calls.
///
/// # Arguments
///
/// * `recipient` - The HWND of the recipient window as a WPARAM.
/// * `sender` - The sender's window handle as a HWND.
/// * `game_event` - The event identifier as a usize.
/// * `payload` - The data payload to be sent as a slice of bytes.
#[cfg(windows)]
unsafe fn send_response_message(
    recipient: WPARAM,
    sender: HWND,
    game_event: usize,
    payload: &[u8],
) {
    info!(
        "Sending response message - Event: {}, Payload length: {}",
        game_event,
        payload.len()
    );
    let copy_data = COPYDATASTRUCT {
        dwData: game_event,
        cbData: payload.len() as u32,
        lpData: payload.as_ptr() as *mut _,
    };
    let result = SendMessageW(
        recipient as HWND,
        WM_COPYDATA,
        sender as WPARAM,
        &copy_data as *const _ as LPARAM,
    );
    info!("SendMessageW result: {}", result);
}

/// Handles the account name request from the game client.
///
/// This function retrieves the account name and sends it back to the game client.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers and Windows API calls.
///
/// # Arguments
///
/// * `recipient` - The HWND of the recipient window as a WPARAM.
/// * `sender` - The sender's window handle as a HWND.
#[cfg(windows)]
unsafe fn handle_account_name_request(recipient: WPARAM, sender: HWND) {
    let account_name = GLOBAL_CREDENTIALS.get_account_name();
    if cfg!(debug_assertions) {
        info!("Account Name Request - Sending: {}", account_name);
    } else {
        info!("Account Name Request");
    }
    let account_name_utf16: Vec<u8> = account_name
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes().to_vec())
        .collect();
    send_response_message(recipient, sender, 2, &account_name_utf16);
    info!("Game event 2 (LAUNCHER_GAME_EVENT_ACCOUNT_NAME_RESPONSE) sended");
}

/// Handles the session ticket request from the game client.
///
/// This function retrieves the session ticket and sends it back to the game client.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers and Windows API calls.
///
/// # Arguments
///
/// * `recipient` - The HWND of the recipient window as a WPARAM.
/// * `sender` - The sender's window handle as a HWND.
#[cfg(windows)]
unsafe fn handle_session_ticket_request(recipient: WPARAM, sender: HWND) {
    let session_ticket = GLOBAL_CREDENTIALS.get_ticket();
    if cfg!(debug_assertions) {
        info!("Session Ticket Request - Sending: {}", session_ticket);
    } else {
        info!("Session Ticket Request");
    }
    send_response_message(recipient, sender, 4, session_ticket.as_bytes());
    info!("Game event 4 (LAUNCHER_GAME_EVENT_SESSION_TICKET_RESPONSE) sended");
}

/// Handles the server list request from the game client.
///
/// This function retrieves the server list asynchronously and sends it back to the game client.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers and Windows API calls.
///
/// # Arguments
///
/// * `recipient` - The HWND of the recipient window as a WPARAM.
/// * `sender` - The sender's window handle as a usize.
#[cfg(windows)]
unsafe fn handle_server_list_request(recipient: WPARAM, sender: usize) {
    let runtime = Runtime::new().expect("Failed to create Tokio runtime");
    let server_list_data =
        runtime.block_on(async { get_server_list().await.expect("Failed to get server list") });
    send_response_message(recipient, sender as HWND, 6, &server_list_data);
    info!("Game event 6 (LAUNCHER_GAME_EVENT_SERVER_LIST_RESPONSE) sended");
}

/// Handles the event of entering a lobby or world.
///
/// This function processes the payload to determine if the player is entering a lobby or a specific world,
/// and sends an appropriate response.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers and Windows API calls.
///
/// # Arguments
///
/// * `recipient` - The HWND of the recipient window as a WPARAM.
/// * `sender` - The HWND of the sender window.
/// * `payload` - The payload containing world information, if any.
#[cfg(windows)]
unsafe fn handle_enter_lobby_or_world(recipient: WPARAM, sender: HWND, payload: &[u8]) {
    if payload.is_empty() {
        on_lobby_entered();
        send_response_message(recipient, sender, 8, &[]);
    } else {
        let world_name = String::from_utf8_lossy(payload);
        on_world_entered(&world_name);
        send_response_message(recipient, sender, 8, payload);
    }
}

/// Handles the "Open Website" game command (0x19).
///
/// This function is called when the game requests to open a website
/// in the system's default web browser.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers and `transmute`.
///
/// # Arguments
///
/// * `_recipient` - The HWND of the recipient window as a WPARAM (unused).
/// * `_sender` - The HWND of the sender window (unused).
/// * `payload` - The payload containing the `LauncherOpenWebsiteCommand` data.
#[cfg(windows)]
unsafe fn handle_open_website_command(_recipient: WPARAM, _sender: HWND, payload: &[u8]) {
    let event_name = "LAUNCHER_GAME_OPEN_WEBSITE_COMMAND";
    
    // ... (payload size check)
    if payload.len() != std::mem::size_of::<u32>() {
        error!(
            "Game event 25 ({}) received with invalid payload size: {} bytes (expected: {} bytes)",
            event_name,
            payload.len(),
            std::mem::size_of::<u32>()
        );
        return;
    }

    // Parse the payload
    let website_id = u32::from_le_bytes(payload.try_into().unwrap());
    let website_id_str = website_id.to_string(); // The ID will be the map key
    
    info!("Game event 25 ({}) received - Website ID: {}", event_name, website_id);
    
    // --- REPLACED LOGIC ---
    
    // 1. Lock the global PAGES_MAP from teralib for reading
    let url_to_open: Option<String> = {
        let map_guard = PAGES_MAP.read().unwrap();
        // 2. Look up the URL using the ID as the key and clone it
        map_guard.get(&website_id_str).cloned()
    };

    // 3. Open the URL if it was found
    if let Some(url) = url_to_open {
        info!("Found URL for ID {}: {}", website_id, url);
        // Format the URL if it contains %s (for authKey, etc.)
        // (Although it's usually unnecessary for PAGES_MAP, it's good practice)
        let final_url = if url.contains("%s") {
            // Note: ACTS_MAP uses %s, PAGES_MAP usually does not.
            // If you also need the AuthKey here, you'll have to store it in GLOBAL_CREDENTIALS
            // and retrieve it. For now, assume PAGES_MAP contains direct URLs.
            url.replace("%s", &GLOBAL_CREDENTIALS.get_ticket())
        } else {
            url
        };

        if let Err(err) = open_website(&final_url) {
            error!("Failed to open website '{}': {}", final_url, err);
        }
    } else {
        info!("No URL found in PAGES_MAP for ID: {}. Ignoring.", website_id);
    }
}

/// Opens a website in the system's default browser.
///
/// Uses platform-specific commands to ensure compatibility across Windows, macOS, and Linux.
#[cfg(windows)]
fn open_website(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()?;
    }

    // #[cfg(target_os = "macos")]
    // {
    //     std::process::Command::new("open")
    //         .arg(url)
    //         .spawn()?;
    // }

    // #[cfg(target_os = "linux")]
    // {
    //     std::process::Command::new("xdg-open")
    //         .arg(url)
    //         .spawn()?;
    // }

    Ok(())
}

// TODO handle_web_url_request & handle_web_url_response

/// Handles the game Web URL Request command (0x1A).
///
/// This function processes the request sent by the game to obtain
/// the corresponding URL based on the UIWindow ID.
///
/// # Safety
///
/// This function is unsafe because it works with raw pointers and unverified payload data.
// unsafe fn handle_web_url_request(_recipient: WPARAM, _sender: HWND, payload: &[u8]) {
//     let event_name = "LauncherWebURLRequest";
//     info!("Game event 26 ({}) received", event_name);
// }

/// Handles the game Web URL Response command (0x1B).
///
/// This function sends the resolved URL back to the game.
///
/// # Safety
///
/// Unsafe due to raw pointer usage.
// unsafe fn handle_web_url_response(_recipient: WPARAM, _sender: HWND, id: u32, url: &str) {
//     let event_name = "LauncherWebURLResponse";
//     info!("Game event 27 ({}) received - ID: {}, URL: {}", event_name, id, url);
// }

/// Handles the game start event.
///
/// This function is called when the game starts. Currently, it only logs the event.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers, but it doesn't perform any unsafe operations.
///
/// # Arguments
///
/// * `_recipient` - The HWND of the recipient window as a WPARAM (unused).
/// * `_sender` - The HWND of the sender window (unused).
/// * `_payload` - The payload associated with the game start event (unused).
#[cfg(windows)]
unsafe fn handle_game_start(_recipient: WPARAM, _sender: HWND, _payload: &[u8]) {
    let event_name = "LAUNCHER_GAME_EVENT_GAME_STARTED";
    info!("Game started");
    info!("Game event 1000 ({}) received", event_name);
}

/// Handles various game events.
///
/// This function is called for various game events identified by the event_id.
/// Currently, it only logs the event.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers, but it doesn't perform any unsafe operations.
///
/// # Arguments
///
/// * `_recipient` - The HWND of the recipient window as a WPARAM (unused).
/// * `_sender` - The HWND of the sender window (unused).
/// * `event_id` - The identifier of the game event.
/// * `_payload` - The payload associated with the game event (unused).
#[cfg(windows)]
unsafe fn handle_game_event(_recipient: WPARAM, _sender: HWND, event_id: usize, _payload: &[u8]) {
    let event_name = match event_id {
        1001 => "LAUNCHER_GAME_EVENT_ENTERED_INTO_CINEMATIC",
        1002 => "LAUNCHER_GAME_EVENT_ENTERED_SERVER_LIST",
        1003 => "LAUNCHER_GAME_EVENT_ENTERING_LOBBY",
        1004 => "LAUNCHER_GAME_EVENT_ENTERED_LOBBY",
        1005 => "LAUNCHER_GAME_EVENT_ENTERING_CHARACTER_CREATION",
        1006 => "LAUNCHER_GAME_EVENT_LEFT_LOBBY",
        1007 => "LAUNCHER_GAME_EVENT_DELETED_CHARACTER",
        1008 => "LAUNCHER_GAME_EVENT_CANCELED_CHARACTER_CREATION",
        1009 => "LAUNCHER_GAME_EVENT_ENTERED_CHARACTER_CREATION",
        1010 => "LAUNCHER_GAME_EVENT_CREATED_CHARACTER",
        1011 => "LAUNCHER_GAME_EVENT_ENTERED_WORLD",
        1012 => "LAUNCHER_GAME_EVENT_FINISHED_LOADING_SCREEN",
        1013 => "LAUNCHER_GAME_EVENT_LEFT_WORLD",
        1014 => "LAUNCHER_GAME_EVENT_MOUNTED_PEGASUS",
        1015 => "LAUNCHER_GAME_EVENT_DISMOUNTED_PEGASUS",
        1016 => "LAUNCHER_GAME_EVENT_CHANGED_CHANNEL",
        _ => "UNKNOWN_LAUNCHER_GAME_EVENT",
    };

    info!("Game event {} ({}) received", event_id, event_name);
}

/// Handles the game exit event.
///
/// This function is called when the game exits normally. Currently, it only logs the event.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers, but it doesn't perform any unsafe operations.
///
/// # Arguments
///
/// * `_recipient` - The HWND of the recipient window as a WPARAM (unused).
/// * `_sender` - The HWND of the sender window (unused).
/// * `_payload` - The payload associated with the game exit event (unused).
#[cfg(windows)]
unsafe fn handle_game_exit(_recipient: WPARAM, _sender: HWND, payload: &[u8]) {
    let event_name = "LAUNCHER_GAME_EVENT_GAME_EXIT";
    info!("Game event 1020 ({}) received", event_name);

    // LauncherGameExitNotification: { length: u32, code: u32, reason: u32 }
    // length must be 12; payload here is the COPYDATASTRUCT data (12 bytes).
    if payload.len() >= 12 {
        let length = u32::from_le_bytes(payload[0..4].try_into().unwrap_or([0;4]));
        let code   = u32::from_le_bytes(payload[4..8].try_into().unwrap_or([0;4]));
        let reason = u32::from_le_bytes(payload[8..12].try_into().unwrap_or([0;4]));
        info!("Game exited — length={}, code={}, reason={} (0x{:x})", length, code, reason, reason);
        if let Ok(mut info) = LAST_EXIT_INFO.lock() {
            *info = GameExitInfo { code, reason };
        }
    } else {
        info!("Game ended (no exit payload)");
        if let Ok(mut info) = LAST_EXIT_INFO.lock() {
            *info = GameExitInfo::default();
        }
    }
}

/// Handles the game crash event.
///
/// This function is called when the game crashes. Currently, it only logs the event as an error.
///
/// # Safety
///
/// This function is unsafe due to its use of raw pointers, but it doesn't perform any unsafe operations.
///
/// # Arguments
///
/// * `_recipient` - The HWND of the recipient window as a WPARAM (unused).
/// * `_sender` - The HWND of the sender window (unused).
/// * `_payload` - The payload associated with the game crash event (unused).
#[cfg(windows)]
unsafe fn handle_game_crash(_recipient: WPARAM, _sender: HWND, payload: &[u8]) {
    let event_name = "LAUNCHER_GAME_EVENT_GAME_CRASH";
    error!("Game crash detected");
    info!("Game event 1021 ({}) received", event_name);

    // LauncherGameCrashNotification: { details: u16string } (not NUL-terminated)
    let details = if payload.len() >= 2 && payload.len() % 2 == 0 {
        let u16_units: Vec<u16> = payload
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        String::from_utf16_lossy(&u16_units).to_string()
    } else {
        String::from_utf8_lossy(payload).to_string()
    };
    error!("Crash details: {}", details);
    if let Ok(mut d) = LAST_CRASH_DETAILS.lock() {
        *d = details;
    }
}

/// Logs the event of entering the lobby.
#[cfg(windows)]
fn on_lobby_entered() {
    info!("Entered the lobby");
}

/// Logs the event of entering a world.
///
/// # Arguments
///
/// * `world_name` - The name of the world being entered.
#[cfg(windows)]
fn on_world_entered(world_name: &str) {
    info!("Entered the world: {}", world_name);
}

/// Asynchronously retrieves the server list.
///
/// This function sends a GET request to a local server to retrieve the server list,
/// then parses the JSON response into a ServerList struct.
///
/// # Returns
///
/// A Result containing a Vec<u8> of the encoded server list on success, or an error on failure.
#[cfg(windows)]
async fn get_server_list() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let url = config::get_config_value("SERVER_LIST_URL");
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("Unsuccessful HTTP response: {}", response.status()).into());
    }

    let json: Value = response.json().await?;
    let server_list = parse_server_list_json(&json)?;

    let mut buf = Vec::new();
    server_list.encode(&mut buf)?;
    Ok(buf)
}

/// Parses JSON into ServerList struct.
///
/// Converts server list JSON to ServerList with error checking.
///
/// # Arguments
///
/// * `json` - Reference to serde_json::Value with server list data.
///
/// # Returns
///
/// Result<ServerList, Box<dyn std::error::Error>>:
/// - Ok(ServerList): Populated ServerList struct
/// - Err: Parsing error description
#[cfg(windows)]
fn parse_server_list_json(json: &Value) -> Result<ServerList, Box<dyn std::error::Error>> {
    let mut server_list = ServerList {
        servers: vec![],
        last_server_id: 0,
        sort_criterion: 2,
    };

    // Parse GLOBAL_CREDENTIALS.get_characters_count()
    let credentials = GLOBAL_CREDENTIALS.get_characters_count();
    info!("Raw credentials string: {}", credentials);

    let parts: Vec<&str> = credentials.split('|').collect();

    let player_last_server = parts.first().unwrap_or(&"0");
    let player_last_server_id = if parts.len() > 1 && !parts[1].is_empty() {
        parts[1]
            .split(',')
            .next()
            .unwrap_or("0")
            .parse::<u32>()
            .unwrap_or(0)
    } else {
        2800
    };

    // Parse character counts for each server
    let character_counts: std::collections::HashMap<u32, u32> = if parts.len() > 1 {
        parts[1]
            .split(',')
            .collect::<Vec<&str>>()
            .chunks(2)
            .filter_map(|chunk| {
                if chunk.len() == 2 {
                    Some((chunk[0].parse::<u32>().ok()?, chunk[1].parse::<u32>().ok()?))
                } else {
                    None
                }
            })
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    info!(
        "Parsed values - Last server: {}, Last server ID: {}, Character counts: {:?}",
        player_last_server, player_last_server_id, character_counts
    );

    let servers = json["servers"]
        .as_array()
        .ok_or("No servers found in JSON")?;
    for server in servers {
        let server_id = server["id"]
            .as_u64()
            .ok_or("Missing or invalid 'id' field")? as u32;
        let character_count = character_counts.get(&server_id).cloned().unwrap_or(0);

        let json_available = server["available"].as_u64().unwrap_or(0);

        info!(
            "Processing server: id={}, name={}, json_available={}",
            server_id, server["name"], json_available
        );

        let display_count = format!("({})", character_count);
        let name = format!(
            "{}{}",
            server["name"]
                .as_str()
                .ok_or("Missing or invalid 'name' field")?,
            display_count
        );
        let title = format!(
            "{}{}",
            server["title"]
                .as_str()
                .ok_or("Missing or invalid 'title' field")?,
            display_count
        );

        info!("Formatted server name: {}", name);

        // Modify population field based on 'available' in JSON
        let population = if json_available == 0 {
            "<b><font color=\"#FF0000\">Offline</font></b>".to_string()
        } else {
            server["population"]
                .as_str()
                .ok_or("Missing or invalid 'population' field")?
                .to_string()
        };

        // Handle address and host fields
        let address_str = server["address"].as_str();
        let host_str = server["host"].as_str();

        let (address, host) = match (address_str, host_str) {
            (Some(addr), Some(_)) => {
                // If both are present, use address and ignore host
                (ipv4_to_u32(addr), Vec::new())
            }
            (Some(addr), None) => (ipv4_to_u32(addr), Vec::new()),
            (None, Some(h)) => (0, utf16_to_bytes(h)),
            (None, None) => return Err("Either 'address' or 'host' must be set".into()),
        };

        let server_info = ServerInfo {
            id: server_id,
            name: utf16_to_bytes(&name),
            category: utf16_to_bytes(
                server["category"]
                    .as_str()
                    .ok_or("Missing or invalid 'category' field")?,
            ),
            title: utf16_to_bytes(&title),
            queue: utf16_to_bytes(
                server["queue"]
                    .as_str()
                    .ok_or("Missing or invalid 'queue' field")?,
            ),
            population: utf16_to_bytes(&population),
            address,
            port: server["port"]
                .as_u64()
                .ok_or("Missing or invalid 'port' field")? as u32,
            available: 1,
            unavailable_message: utf16_to_bytes(
                server["unavailable_message"].as_str().unwrap_or(""),
            ),
            host,
        };
        server_list.servers.push(server_info);
    }

    server_list.last_server_id = player_last_server_id;
    server_list.sort_criterion = json["sort_criterion"].as_u64().unwrap_or(3) as u32;

    Ok(server_list)
}

/// Converts a Rust string to UTF-16 little-endian bytes.
///
/// This function is useful for preparing strings for Windows API calls that expect UTF-16.
///
/// # Arguments
///
/// * `s` - A string slice that holds the text to be converted.
///
/// # Returns
///
/// A vector of bytes representing the UTF-16 little-endian encoded string.
#[cfg(windows)]
fn utf16_to_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .flat_map(|c| c.to_le_bytes().to_vec())
        .collect()
}

/// Converts an IPv4 address string to a u32 representation.
///
/// # Arguments
///
/// * `ip` - A string slice that holds the IPv4 address.
///
/// # Returns
///
/// A u32 representation of the IP address, or 0 if parsing fails.
#[cfg(windows)]
fn ipv4_to_u32(ip: &str) -> u32 {
    ip.parse::<std::net::Ipv4Addr>()
        .map(|addr| u32::from_be_bytes(addr.octets()))
        .unwrap_or(0)
}
