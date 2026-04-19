// launcher-bridge: Win32 IPC bridge for the TERA launcher on Linux.
//
// This binary runs under Wine alongside Tera.exe and bridges the Win32 WM_COPYDATA
// IPC protocol to the native Linux launcher via stdin/stdout JSON pipes.
//
// Protocol:
//   stdin  (once): JSON credentials object (account_name, ticket, game_path, ...)
//   stdout (lines): JSON event objects { "event": "...", ... }
//     - { "event": "window_ready" }
//     - { "event": "game_started", "pid": 1234 }
//     - { "event": "open_website", "url": "https://..." }
//     - { "event": "game_exited", "code": 0 }

#![windows_subsystem = "console"]

use lazy_static::lazy_static;
use log::{error, info};
use once_cell::sync::Lazy;
use prost::Message;
use reqwest;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    ffi::OsStr,
    io::{self, Read, Write},
    os::windows::ffi::OsStrExt,
    ptr::null_mut,
    slice,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::{runtime::Runtime, sync::{mpsc, Notify}, process::Command};
use winapi::{
    shared::{
        minwindef::{BOOL, LPARAM, LRESULT, TRUE, UINT, WPARAM},
        windef::HWND,
    },
    um::{
        errhandlingapi::GetLastError,
        libloaderapi::GetModuleHandleW,
        winuser::{GetClassNameW, *},
    },
};

// Include the pre-generated prost server list structs
mod serverlist {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/_serverlist_proto.rs"));
}
use serverlist::{server_list::ServerInfo, ServerList};

// ─── Windows constants ────────────────────────────────────────────────────────
const WM_GAME_EXITED: u32 = WM_USER + 1;

// ─── Credentials received from stdin ─────────────────────────────────────────
#[derive(Debug, Deserialize)]
struct BridgeCredentials {
    account_name: String,
    characters_count: String,
    ticket: String,
    game_lang: String,
    /// Full Wine-compatible path to Tera.exe, e.g. Z:\path\to\Binaries\Tera.exe
    game_path: String,
    server_list_url: String,
    acts_map: HashMap<String, String>,
    pages_map: HashMap<String, String>,
}

// ─── Global state ─────────────────────────────────────────────────────────────
lazy_static! {
    static ref ACCOUNT_NAME: RwLock<String> = RwLock::new(String::new());
    static ref CHARACTERS_COUNT: RwLock<String> = RwLock::new(String::new());
    static ref TICKET: RwLock<String> = RwLock::new(String::new());
    static ref GAME_LANG: RwLock<String> = RwLock::new(String::new());
    static ref GAME_PATH: RwLock<String> = RwLock::new(String::new());
    static ref SERVER_LIST_URL: RwLock<String> = RwLock::new(String::new());
    static ref PAGES_MAP: RwLock<HashMap<String, String>> = RwLock::new(HashMap::new());
    static ref SERVER_LIST_SENDER: Mutex<Option<mpsc::Sender<(WPARAM, usize)>>> =
        Mutex::new(None);
}

static WINDOW_HANDLE: Lazy<Mutex<Option<SafeHWND>>> = Lazy::new(|| Mutex::new(None));

// ─── SafeHWND wrapper ─────────────────────────────────────────────────────────
#[derive(Clone, Copy)]
struct SafeHWND(HWND);
unsafe impl Send for SafeHWND {}
unsafe impl Sync for SafeHWND {}
impl SafeHWND {
    fn new(hwnd: HWND) -> Self { SafeHWND(hwnd) }
    fn get(&self) -> HWND { self.0 }
}

// ─── Helper: write a JSON event line to stdout ────────────────────────────────
fn emit(event: &serde_json::Value) {
    let mut stdout = io::stdout();
    let _ = writeln!(stdout, "{}", event);
    let _ = stdout.flush();
}

// ─── Helper: null-terminated UTF-16 wide string ───────────────────────────────
fn to_wstring(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

// ─── Win32 IPC helpers ────────────────────────────────────────────────────────
unsafe fn send_response_message(
    recipient: WPARAM,
    sender: HWND,
    game_event: usize,
    payload: &[u8],
) {
    let copy_data = COPYDATASTRUCT {
        dwData: game_event,
        cbData: payload.len() as u32,
        lpData: payload.as_ptr() as *mut _,
    };
    SendMessageW(
        recipient as HWND,
        WM_COPYDATA,
        sender as WPARAM,
        &copy_data as *const _ as LPARAM,
    );
}

// ─── Event handlers ──────────────────────────────────────────────────────────
unsafe fn handle_account_name_request(recipient: WPARAM, sender: HWND) {
    let name = ACCOUNT_NAME.read().unwrap().clone();
    let utf16: Vec<u8> = name
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes().to_vec())
        .collect();
    send_response_message(recipient, sender, 2, &utf16);
    info!("Responded to account name request");
}

unsafe fn handle_session_ticket_request(recipient: WPARAM, sender: HWND) {
    let ticket = TICKET.read().unwrap().clone();
    send_response_message(recipient, sender, 4, ticket.as_bytes());
    info!("Responded to session ticket request");
}

async fn handle_server_list_request_async(recipient: WPARAM, sender: usize) {
    match get_server_list().await {
        Ok(data) => {
            unsafe { send_response_message(recipient as WPARAM, sender as HWND, 6, &data) };
            info!("Responded to server list request");
        }
        Err(e) => error!("Server list request failed: {}", e),
    }
}

unsafe fn handle_enter_lobby_or_world(recipient: WPARAM, sender: HWND, payload: &[u8]) {
    if payload.is_empty() {
        info!("Entered lobby");
        send_response_message(recipient, sender, 8, &[]);
    } else {
        let world = String::from_utf8_lossy(payload);
        info!("Entered world: {}", world);
        send_response_message(recipient, sender, 8, payload);
    }
}

unsafe fn handle_open_website_command(_recipient: WPARAM, _sender: HWND, payload: &[u8]) {
    if payload.len() != std::mem::size_of::<u32>() {
        error!("Invalid open_website payload size: {}", payload.len());
        return;
    }
    let website_id = u32::from_le_bytes(payload.try_into().unwrap());
    let id_str = website_id.to_string();

    let url = {
        let map = PAGES_MAP.read().unwrap();
        map.get(&id_str).cloned()
    };

    if let Some(url) = url {
        let ticket = TICKET.read().unwrap().clone();
        let final_url = if url.contains("%s") {
            url.replace("%s", &ticket)
        } else {
            url
        };
        // Emit to native launcher which will call xdg-open
        emit(&serde_json::json!({ "event": "open_website", "url": final_url }));
    } else {
        info!("No URL in PAGES_MAP for id {}", website_id);
    }
}

// ─── Window procedure ─────────────────────────────────────────────────────────
unsafe extern "system" fn wnd_proc(
    h_wnd: HWND,
    msg: UINT,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let copy_data = &*(l_param as *const COPYDATASTRUCT);
            let event_id = copy_data.dwData;
            let payload = if copy_data.cbData > 0 {
                slice::from_raw_parts(copy_data.lpData as *const u8, copy_data.cbData as usize)
            } else {
                &[]
            };
            info!("WM_COPYDATA event_id={}", event_id);
            match event_id {
                1 => handle_account_name_request(w_param, h_wnd),
                3 => handle_session_ticket_request(w_param, h_wnd),
                5 => {
                    // Route via channel so it's handled on the async runtime
                    if let Ok(guard) = SERVER_LIST_SENDER.lock() {
                        if let Some(tx) = &*guard {
                            let _ = tx.blocking_send((w_param, h_wnd as usize));
                        }
                    }
                }
                7 => handle_enter_lobby_or_world(w_param, h_wnd, payload),
                25 => handle_open_website_command(w_param, h_wnd, payload),
                1000 => info!("Game started (event 1000)"),
                1001..=1016 => info!("Game event {}", event_id),
                1020 => info!("Game exit (event 1020)"),
                1021 => info!("Game crash (event 1021)"),
                _ => {}
            }
            1
        }
        WM_GAME_EXITED => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(h_wnd, msg, w_param, l_param),
    }
}

// ─── Win32 window creation + message loop ────────────────────────────────────
unsafe fn create_and_run_game_window(tcs: Arc<Notify>) {
    let class_name = to_wstring("LAUNCHER_CLASS");
    let window_name = to_wstring("LAUNCHER_WINDOW");

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

    if RegisterClassExW(&wnd_class) == 0 {
        error!("Failed to register window class");
        return;
    }

    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        window_name.as_ptr(),
        0, 0, 0, 0, 0,
        null_mut(), null_mut(),
        GetModuleHandleW(null_mut()),
        null_mut(),
    );
    if hwnd.is_null() {
        error!("Failed to create window, error={}", GetLastError());
        UnregisterClassW(class_name.as_ptr(), GetModuleHandleW(null_mut()));
        return;
    }

    if let Ok(mut h) = WINDOW_HANDLE.lock() {
        h.replace(SafeHWND::new(hwnd));
    }

    // Signal to main async task that the window is ready
    tcs.notify_one();

    // Message loop
    let mut msg = std::mem::zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        if msg.message == WM_GAME_EXITED {
            break;
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    DestroyWindow(hwnd);
    UnregisterClassW(class_name.as_ptr(), GetModuleHandleW(null_mut()));

    // Clean up any remaining windows with this class
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut name: [u16; 256] = [0; 256];
        let len = GetClassNameW(hwnd, name.as_mut_ptr(), 256) as usize;
        let search = slice::from_raw_parts(lparam as *const u16, 256);
        let slen = search.iter().position(|&c| c == 0).unwrap_or(256);
        if name[..len].starts_with(&search[..slen]) {
            DestroyWindow(hwnd);
        }
        TRUE
    }
    EnumWindows(Some(enum_proc), class_name.as_ptr() as LPARAM);
}

// ─── Server list fetching ────────────────────────────────────────────────────
async fn get_server_list() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let url = SERVER_LIST_URL.read().unwrap().clone();
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
    }

    let json: Value = response.json().await?;
    let characters_count = CHARACTERS_COUNT.read().unwrap().clone();
    let server_list = parse_server_list_json(&json, &characters_count)?;

    let mut buf = Vec::new();
    server_list.encode(&mut buf)?;
    Ok(buf)
}

fn parse_server_list_json(
    json: &Value,
    characters_count: &str,
) -> Result<ServerList, Box<dyn std::error::Error>> {
    let mut server_list = ServerList {
        servers: vec![],
        last_server_id: 0,
        sort_criterion: 2,
    };

    let parts: Vec<&str> = characters_count.split('|').collect();
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

    let character_counts: HashMap<u32, u32> = if parts.len() > 1 {
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
        HashMap::new()
    };

    let servers = json["servers"]
        .as_array()
        .ok_or("No 'servers' array in JSON")?;

    for server in servers {
        let server_id = server["id"]
            .as_u64()
            .ok_or("Missing or invalid 'id' field")? as u32;
        let char_count = character_counts.get(&server_id).cloned().unwrap_or(0);
        let display_count = format!("({})", char_count);

        let json_available = server["available"].as_u64().unwrap_or(0);

        let name = format!(
            "{}{}",
            server["name"].as_str().ok_or("Missing 'name'")?,
            display_count
        );
        let title = format!(
            "{}{}",
            server["title"].as_str().ok_or("Missing 'title'")?,
            display_count
        );

        let population = if json_available == 0 {
            "<b><font color=\"#FF0000\">Offline</font></b>".to_string()
        } else {
            server["population"]
                .as_str()
                .ok_or("Missing 'population'")?
                .to_string()
        };

        let address_str = server["address"].as_str();
        let host_str = server["host"].as_str();

        let (address, host) = match (address_str, host_str) {
            (Some(addr), _) => (ipv4_to_u32(addr), Vec::new()),
            (None, Some(h)) => (0, utf16_to_bytes(h)),
            (None, None) => return Err("Either 'address' or 'host' must be set".into()),
        };

        server_list.servers.push(ServerInfo {
            id: server_id,
            name: utf16_to_bytes(&name),
            category: utf16_to_bytes(server["category"].as_str().ok_or("Missing 'category'")?),
            title: utf16_to_bytes(&title),
            queue: utf16_to_bytes(server["queue"].as_str().ok_or("Missing 'queue'")?),
            population: utf16_to_bytes(&population),
            address,
            port: server["port"].as_u64().ok_or("Missing 'port'")? as u32,
            available: 1,
            unavailable_message: utf16_to_bytes(
                server["unavailable_message"].as_str().unwrap_or(""),
            ),
            host,
        });
    }

    server_list.last_server_id = player_last_server_id;
    server_list.sort_criterion = json["sort_criterion"].as_u64().unwrap_or(3) as u32;
    Ok(server_list)
}

fn utf16_to_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .flat_map(|c| c.to_le_bytes().to_vec())
        .collect()
}

fn ipv4_to_u32(ip: &str) -> u32 {
    let parts: Vec<u32> = ip.split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    if parts.len() == 4 {
        (parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3]
    } else {
        0
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────
fn main() {
    // Read the JSON credentials from stdin synchronously before entering async runtime.
    // The native launcher writes this and then closes stdin.
    let mut raw = String::new();
    io::stdin()
        .read_to_string(&mut raw)
        .expect("Failed to read credentials from stdin");

    let creds: BridgeCredentials =
        serde_json::from_str(&raw).expect("Failed to parse credentials JSON from stdin");

    // Populate globals
    *ACCOUNT_NAME.write().unwrap() = creds.account_name;
    *CHARACTERS_COUNT.write().unwrap() = creds.characters_count;
    *TICKET.write().unwrap() = creds.ticket;
    *GAME_LANG.write().unwrap() = creds.game_lang.clone();
    *GAME_PATH.write().unwrap() = creds.game_path.clone();
    *SERVER_LIST_URL.write().unwrap() = creds.server_list_url;
    *PAGES_MAP.write().unwrap() = creds.pages_map;

    // Set up server list channel
    let (tx, mut rx) = mpsc::channel::<(WPARAM, usize)>(32);
    *SERVER_LIST_SENDER.lock().unwrap() = Some(tx);

    // Build a tokio runtime for the async parts
    let rt = Runtime::new().expect("Failed to create tokio runtime");

    rt.block_on(async move {
        // Spawn the Win32 message window in a blocking thread
        let tcs = Arc::new(Notify::new());
        let tcs_clone = Arc::clone(&tcs);
        let _win_handle =
            tokio::task::spawn_blocking(move || unsafe { create_and_run_game_window(tcs_clone) });

        // Handle server list requests asynchronously
        tokio::spawn(async move {
            while let Some((w_param, sender)) = rx.recv().await {
                handle_server_list_request_async(w_param, sender).await;
            }
        });

        // Wait for the Win32 window to be ready
        tcs.notified().await;
        emit(&serde_json::json!({ "event": "window_ready" }));

        // Launch Tera.exe
        let game_path = GAME_PATH.read().unwrap().clone();
        let game_lang = GAME_LANG.read().unwrap().clone();

        let mut child = match Command::new(&game_path)
            .arg(format!("-LANGUAGEEXT={}", game_lang))
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                emit(&serde_json::json!({
                    "event": "error",
                    "message": format!("Failed to spawn Tera.exe: {}", e)
                }));
                return;
            }
        };

        let pid = child.id().unwrap_or(0);
        emit(&serde_json::json!({ "event": "game_started", "pid": pid }));

        let code = match child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        };

        emit(&serde_json::json!({ "event": "game_exited", "code": code }));

        // Signal the Win32 window to close
        if let Ok(guard) = WINDOW_HANDLE.lock() {
            if let Some(hwnd) = *guard {
                unsafe {
                    PostMessageW(hwnd.get(), WM_GAME_EXITED, 0, 0);
                }
            }
        }
    });
}
