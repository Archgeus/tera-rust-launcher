#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Standard library imports
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

// Third-party imports
use dotenv::dotenv;
use log::{LevelFilter, error, info};
use tokio::sync::{watch, Mutex, mpsc};
use tokio::io::AsyncWriteExt;
use rayon::prelude::*;
use tokio::runtime::Runtime;
use serde::{Deserialize, Serialize};
use serde_json::{json};
use tauri::{Manager};
use tauri::api::dialog::FileDialogBuilder;
use teralib::{get_game_status_receiver, run_game, reset_global_state, get_last_exit_info, get_last_crash_details, get_last_game_stderr};
use teralib::config::get_config_value;
use reqwest::Client;
use lazy_static::lazy_static;
use ini::Ini;
use sha2::{Sha256, Digest};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use walkdir::WalkDir;
use reqwest::cookie::Jar;
use reqwest::cookie::CookieStore;
use url::Url;

// Struct definitions

struct GlobalAuthInfo {
  character_count: String,
  user_no: i32,
  user_name: String,
  auth_key: String,
}

lazy_static! {
  static ref GLOBAL_AUTH_INFO: RwLock<GlobalAuthInfo> = RwLock::new(GlobalAuthInfo {
    character_count: String::new(),
    user_no: 0,
    user_name: String::new(),
    auth_key: String::new(),
  });

  static ref AUTHENTICATED_CLIENT: Mutex<Option<Client>> = Mutex::new(None);

  static ref SIGNUP_SESSION_CLIENT: Mutex<Option<Client>> = Mutex::new(None);

  static ref LAUNCHER_BASE_URL: String = get_config_value("LAUNCHER_ACTION_URL");

  static ref GLOBAL_ACTS_MAP: RwLock<HashMap<String, String>> = RwLock::new(HashMap::new());
  static ref GLOBAL_PAGES_MAP: RwLock<HashMap<String, String>> = RwLock::new(HashMap::new());
}

// Struct for the initial /launcher/LoginAction response
#[derive(Deserialize)]
struct InitialLoginResponse {
  #[serde(rename = "Return")]
  return_value: bool,
  #[serde(rename = "Msg")]
  msg: String,
  #[serde(rename = "ReturnCode")]
  return_code: i32,
}

// Struct for the /launcher/GetAccountInfoAction response
#[derive(Deserialize)]
struct AccountInfoResponse {
  #[serde(rename = "UserNo")]
  user_no: i32,
  #[serde(rename = "UserName")]
  user_name: String,
  #[serde(rename = "Permission")]
  permission: i32,
  #[serde(rename = "Privilege")]
  privilege: i32,
  #[serde(rename = "Banned", default)] 
  banned: bool,
}

// Struct for the /launcher/GetAuthKeyAction response
#[derive(Deserialize)]
struct AuthKeyResponse {
  #[serde(rename = "AuthKey")]
  auth_key: String,
}

// Struct for the /launcher/GetCharacterCountAction response
#[derive(Deserialize)]
struct CharCountResponse {
  #[serde(rename = "CharacterCount")]
  character_count: String,
}

// Struct for the /launcher/GetMaintenanceStatusAction response
#[derive(Deserialize, Debug, Clone, Serialize)]
struct MaintenanceResponse {
  #[serde(rename = "Return")]
  return_value: bool,
  #[serde(rename = "ReturnCode")]
  return_code: i32,
  #[serde(rename = "Msg")]
  msg: String,
  #[serde(rename = "StartTime")]
  start_time: Option<u64>,
  #[serde(rename = "EndTime")]
  end_time: Option<u64>,
}

// Struct for GET /launcher/GetCaptcha verify response
#[derive(Deserialize)]
struct CaptchaVerifyApiResponse {
    verified: bool,
}

// Struct for POST /launcher/SignupAction response
#[derive(Deserialize, Serialize)]
struct SignupApiResponse {
    #[serde(rename = "Return")]
    return_value: bool,
    #[serde(rename = "ReturnCode")]
    return_code: i32,
    #[serde(rename = "Msg")]
    msg: String,
}

// Struct for GET /launcher/GetPortalConfig response
#[derive(Deserialize, Serialize)]
struct PortalConfigResponse {
    #[serde(rename = "registrationDisabled", default)]
    registration_disabled: bool,
    #[serde(rename = "captchaEnabled", default)]
    captcha_enabled: bool,
    #[serde(rename = "patchNoCheck", default)]
    patch_no_check: bool,
}

// This struct combines all info into the format the frontend expects (same as old LoginResponse)
#[derive(Serialize)]
struct CombinedLoginResponse {
  #[serde(rename = "Return")]
  return_value: bool,
  #[serde(rename = "ReturnCode")]
  return_code: i32,
  #[serde(rename = "Msg")]
  msg: String,
  #[serde(rename = "CharacterCount")]
  character_count: String,
  #[serde(rename = "Permission")]
  permission: i32,
  #[serde(rename = "Privilege")]
  privilege: i32,
  #[serde(rename = "UserNo")]
  user_no: i32,
  #[serde(rename = "UserName")]
  user_name: String,
  #[serde(rename = "AuthKey")]
  auth_key: String,
  #[serde(rename = "Banned")]
  banned: bool,

  #[serde(rename = "ActsMap", skip_serializing_if = "Option::is_none")]
    acts_map: Option<serde_json::Value>,
    #[serde(rename = "PagesMap", skip_serializing_if = "Option::is_none")]
    pages_map: Option<serde_json::Value>,

  session_cookie: Option<String>,
}

/* const CONFIG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/config.json"));

lazy_static::lazy_static! {
  static ref CONFIG_JSON: Value = serde_json::from_str(CONFIG).expect("Failed to parse config");
} */


#[derive(Debug, Serialize, Deserialize, Clone)]
struct FileInfo {
  path: String,
  hash: String,
  size: u64,
  url: String,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
  file_name: String,
  progress: f64,
  speed: f64,
  downloaded_bytes: u64,
  total_bytes: u64,
  total_files: usize,
  elapsed_time: f64,
  current_file_index: usize,
}

#[derive(Clone, Serialize)]
struct FileCheckProgress {
  current_file: String,
  progress: f64,
  current_count: usize,
  total_files: usize,
  elapsed_time: f64,
  files_to_update: usize,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GetFilesToUpdateParams {
  force: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CachedFileInfo {
  hash: String,
  last_modified: SystemTime,
}

struct GameState {
  status_receiver: Arc<Mutex<watch::Receiver<bool>>>,
  is_launching: Arc<Mutex<bool>>,
}


//static INIT: Once = Once::new();


lazy_static! {
  static ref HASH_CACHE: Mutex<HashMap<String, CachedFileInfo>> = Mutex::new(HashMap::new());
}


/* fn get_config_value(key: &str) -> String {
  CONFIG_JSON[key].as_str().expect(&format!("{} must be set in config.json", key)).to_string()
} */

fn is_ignored(path: &Path, game_path: &Path, ignored_paths: &HashSet<&str>) -> bool {
  let relative_path = path.strip_prefix(game_path).unwrap().to_str().unwrap().replace("\\", "/");

  // Ignore files at the root
  if relative_path.chars().filter(|&c| c == '/').count() == 0 {
    return true;
  }

  // Check if the path is in the list of ignored paths
  for ignored_path in ignored_paths {
    if relative_path.starts_with(ignored_path) {
      return true;
    }
  }

  false
}

async fn get_server_hash_file() -> Result<serde_json::Value, String> {
  let client = reqwest::Client::new();
  let res = client
    .get(get_hash_file_url())
    .send().await
    .map_err(|e| e.to_string())?;
  let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
  Ok(json)
}


fn calculate_file_hash<P: AsRef<Path>>(path: P) -> Result<String, String> {
  let mut file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
  let mut hasher = Sha256::new();
  let mut buffer = [0; 1024];

  loop {
    let bytes_read = file.read(&mut buffer).map_err(|e| format!("Failed to read file: {}", e))?;
    if bytes_read == 0 {
      break;
    }
    hasher.update(&buffer[..bytes_read]);
  }

  let result = hasher.finalize();
  Ok(format!("{:x}", result))
}

fn get_cache_file_path() -> Result<PathBuf, String> {
  // Get the directory where config.ini is located
  let config_path = find_config_file()
    .ok_or("Config file not found - cannot determine cache directory")?;
  
  // Get the parent directory of config.ini
  let cache_dir = config_path.parent()
    .ok_or("Failed to get config directory")?;
  
  // Create cache file path in the same directory as config.ini
  Ok(cache_dir.join("file_cache.json"))
}

fn save_cache_to_disk(cache: &HashMap<String, CachedFileInfo>) -> Result<(), String> {
  let cache_path = get_cache_file_path()?;
  let serialized = serde_json::to_string(cache).map_err(|e| e.to_string())?;
  let mut file = File::create(cache_path).map_err(|e| e.to_string())?;
  file.write_all(serialized.as_bytes()).map_err(|e| e.to_string())?;
  Ok(())
}

fn load_cache_from_disk() -> Result<HashMap<String, CachedFileInfo>, String> {
  let cache_path = get_cache_file_path()?;
  let mut file = File::open(cache_path).map_err(|e| e.to_string())?;
  let mut contents = String::new();
  file.read_to_string(&mut contents).map_err(|e| e.to_string())?;
  let cache: HashMap<String, CachedFileInfo> = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
  Ok(cache)
}


fn get_hash_file_url() -> String {
  get_config_value("HASH_FILE_URL")
}

fn find_config_file() -> Option<PathBuf> {
  // Prefer config.ini next to the executable — stable regardless of cwd
  if let Ok(exe_path) = env::current_exe() {
    if let Some(exe_dir) = exe_path.parent() {
      let config_in_exe_dir = exe_dir.join("config.ini");
      if config_in_exe_dir.exists() {
        return Some(config_in_exe_dir);
      }
    }
  }

  let current_dir = env::current_dir().ok()?;
  let config_in_current = current_dir.join("config.ini");
  if config_in_current.exists() {
    return Some(config_in_current);
  }

  let parent_dir = current_dir.parent()?;
  let config_in_parent = parent_dir.join("config.ini");
  if config_in_parent.exists() {
    return Some(config_in_parent);
  }

  None
}

/// Get the default configuration file path (in the launcher directory)
fn get_default_config_path() -> Result<PathBuf, String> {
  let exe_path = env::current_exe()
    .map_err(|e| format!("Failed to get launcher directory: {}", e))?;
  
  let exe_dir = exe_path.parent()
    .ok_or("Failed to get launcher parent directory")?;
  
  Ok(exe_dir.join("config.ini"))
}

/// Create a default config file if it doesn't exist
fn create_default_config(config_path: &PathBuf) -> Result<(), String> {
  let exe_path = env::current_exe()
    .map_err(|e| format!("Failed to get launcher directory: {}", e))?;
  
  let exe_dir = exe_path.parent()
    .ok_or("Failed to get launcher parent directory")?;
  
  let mut conf = Ini::new();
  conf.with_section(Some("game"))
    .set("lang", "EUR")
    .set("path", exe_dir.to_str().ok_or("Invalid launcher path")?);

  let mut file = File::create(&config_path)
    .map_err(|e| format!("Failed to create config file: {}", e))?;

  conf.write_to(&mut file)
    .map_err(|e| format!("Failed to write config: {}", e))?;
  
  info!("Created default config.ini at {:?}", config_path);
  
  Ok(())
}

fn load_config() -> Result<(PathBuf, String), String> {
  // Try to find existing config file
  let config_path = if let Some(path) = find_config_file() {
    path
  } else {
    // If not found, create default config at launcher directory
    let default_path = get_default_config_path()?;
    create_default_config(&default_path)?;
    default_path
  };

  let conf = Ini::load_from_file(&config_path).map_err(|e|
    format!("Failed to load config: {}", e)
  )?;

  let section = conf.section(Some("game")).ok_or("Game section not found in config")?;

  let game_path = section.get("path").ok_or("Game path not found in config")?;

  let game_path = PathBuf::from(game_path);

  let game_lang = section.get("lang").ok_or("Game language not found in config")?.to_string();

  Ok((game_path, game_lang))
}

/* fn save_config(game_path: &Path, game_lang: &str) -> Result<(), String> {
  let config_path = find_config_file().ok_or("Config file not found")?;
  let mut conf = Ini::new();

  conf.with_section(Some("game")).set("path", game_path.to_str().ok_or("Invalid game path")?);
  conf.with_section(Some("game")).set("lang", game_lang);

  let mut file = std::fs::File
    ::create(&config_path)
    .map_err(|e| format!("Failed to create config file: {}", e))?;

  conf.write_to(&mut file).map_err(|e| format!("Failed to write config: {}", e))?;

  Ok(())
} */

async fn get_maintenance_status() -> Result<MaintenanceResponse, String> { 
  let client = reqwest::Client::new();
  let base_url = &*LAUNCHER_BASE_URL; 
  let maintenance_url = format!("{}/launcher/GetMaintenanceStatusAction", base_url);

  let res = client
    .get(&maintenance_url)
    .send().await
    .map_err(|e| format!("Failed to connect to maintenance server: {}", e))?;

  if !res.status().is_success() {
    return Err(format!("Maintenance check request failed with status: {}", res.status()));
  }

  let maintenance_body: MaintenanceResponse = res
    .json()
    .await
    .map_err(|e| format!("Failed to parse maintenance response: {}", e))?;

  if !maintenance_body.return_value {
    return Err(format!("Maintenance check API error: {}", maintenance_body.msg));
  }

  Ok(maintenance_body)
}

#[tauri::command]
async fn check_maintenance_and_notify(window: tauri::Window) -> Result<bool, String> {
  match get_maintenance_status().await {
    Ok(response) => {
      let is_maintenance = response.start_time.is_some() || response.end_time.is_some();

      if is_maintenance {
        // Emit the event with full maintenance details for the modal
        let payload = serde_json::to_value(&response)
          .unwrap_or(json!({"msg": "Active maintenance"}));

        if let Err(e) = window.emit("maintenance_active", payload) {
          error!("Failed to emit maintenance_active event: {:?}", e);
        }
      }

      // Return 'true' if maintenance is active, 'false' otherwise
      Ok(is_maintenance)
    }
    Err(e) => {
      error!("Error checking maintenance status: {:?}", e);
      // Return a specific error so the frontend can handle it as a network issue
      Err(format!("ERROR_NETWORK_CHECK: {}", e))
    }
  }
}


#[tauri::command]
async fn generate_hash_file(window: tauri::Window) -> Result<String, String> {
  let start_time = Instant::now();

  let game_path = get_game_path().map_err(|e| e.to_string())?;
  info!("Game path: {:?}", game_path);
  let output_path = game_path.join("hash-file.json");
  info!("Output path: {:?}", output_path);

  // List of files and directories to ignore
  let ignored_paths: HashSet<&str> = [
    "$Patch",
    "Binaries/cookies.dat",
    "Binaries/awesomium.log",
    "S1Game/GuildFlagUpload",
    "S1Game/GuildLogoUpload",
    "S1Game/ImageCache",
    "S1Game/Logs",
    "S1Game/Screenshots",
    "S1Game/Config/S1Engine.ini",
    "S1Game/Config/S1Game.ini",
    "S1Game/Config/S1Input.ini",
    "S1Game/Config/S1Lightmass.ini",
    "S1Game/Config/S1Option.ini",
    "S1Game/Config/S1SystemSettings.ini",
    "S1Game/Config/S1TBASettings.ini",
    "S1Game/Config/S1UI.ini",
    "Launcher.exe",
    "local.db",
    "version.ini",
    "unins000.dat",
    "unins000.exe",
    "config.ini",
    "file_cache.json",
    "hash-file.json",
    "teralauncher.exe",
  ].iter().cloned().collect();

  let total_files = WalkDir::new(&game_path)
    .into_iter()
    .filter_map(|e| e.ok())
    .filter(|e| e.file_type().is_file())
    .filter(|e| !is_ignored(e.path(), &game_path, &ignored_paths))
    .count();
  info!("Total files to process: {}", total_files);

  let progress_bar = ProgressBar::new(total_files as u64);
  let progress_style = ProgressStyle::default_bar()
    .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
    .map_err(|e| e.to_string())?
    .progress_chars("##-");
  progress_bar.set_style(progress_style);

  let processed_files = AtomicU64::new(0);
  let total_size = AtomicU64::new(0);
  let files = Arc::new(Mutex::new(Vec::new()));

  let result: Result<(), String> = WalkDir::new(&game_path)
    .into_iter()
    .par_bridge()
    .try_for_each(|entry| -> Result<(), String> {
      let entry = entry.map_err(|e| e.to_string())?;
      let path = entry.path();
      if path.is_file() && !is_ignored(path, &game_path, &ignored_paths) {
        let relative_path = path.strip_prefix(&game_path).unwrap().to_str().unwrap().replace("\\", "/");
        info!("Processing file: {}", relative_path);

        let contents = std::fs::read(path).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        hasher.update(&contents);
        let hash = format!("{:x}", hasher.finalize());
        let size = contents.len() as u64;
        let file_server_url = get_config_value("FILE_SERVER_URL");
        let url = format!("{}/files/{}", file_server_url, relative_path);

        files.blocking_lock().push(FileInfo {
          path: relative_path.clone(),
          hash,
          size,
          url,
        });

        total_size.fetch_add(size, Ordering::Relaxed);
        let current_processed = processed_files.fetch_add(1, Ordering::Relaxed) + 1;
        progress_bar.set_position(current_processed);

        let progress = (current_processed as f64 / total_files as f64) * 100.0;
        window.emit("hash_file_progress", json!({
          "current_file": relative_path,
          "progress": progress,
          "processed_files": current_processed,
          "total_files": total_files,
          "total_size": total_size.load(Ordering::Relaxed)
        })).map_err(|e| e.to_string())?;
      }
      Ok(())
    });

  if let Err(e) = result {
    error!("Error during file processing: {:?}", e);
    return Err(e);
  }

  progress_bar.finish_with_message("File processing completed");

  info!("Generating JSON");
  let json = serde_json::to_string(&json!({
    "files": files.lock().await.clone()
  })).map_err(|e| e.to_string())?;

  info!("Writing hash file");
  let mut file = File::create(&output_path).map_err(|e| e.to_string())?;
  file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;

  let duration = start_time.elapsed();
  let total_processed = processed_files.load(Ordering::Relaxed);
  let total_size = total_size.load(Ordering::Relaxed);
  info!("Hash file generation completed in {:?}", duration);
  info!("Total files processed: {}", total_processed);
  info!("Total size: {} bytes", total_size);

  Ok(format!("Hash file generated successfully. Processed {} files with a total size of {} bytes in {:?}", total_processed, total_size, duration))
}


#[tauri::command]
async fn select_game_folder() -> Result<String, String> {
  let (tx, mut rx) = mpsc::channel(1);

  FileDialogBuilder::new()
    .set_title("Select Tera Game Folder")
    .set_directory("/")
    .pick_folder(move |folder_path| {
      if let Some(path) = folder_path {
        let _ = tx.try_send(path);
      }
    });

  match rx.recv().await {
    Some(path) => Ok(path.to_string_lossy().into_owned()),
    None => Err("Folder selection cancelled or failed".into()),
  }
}


fn get_game_path() -> Result<PathBuf, String> {
  let (game_path, _) = load_config()?;
  Ok(game_path)
}

/// Find Tera.exe in `binaries_dir` case-insensitively.
/// On Windows the filesystem is case-insensitive so a direct join works.
/// On Linux we scan the directory for a case-insensitive match.
fn find_game_exe(binaries_dir: &PathBuf) -> Option<PathBuf> {
  // Try the canonical name first (fast path, works on Windows and case-correct Linux installs)
  for name in &["Tera.exe", "TERA.exe", "tera.exe"] {
    let candidate = binaries_dir.join(name);
    if candidate.exists() {
      return Some(candidate);
    }
  }
  // Case-insensitive scan (Linux only fallback)
  #[cfg(not(windows))]
  if let Ok(entries) = std::fs::read_dir(binaries_dir) {
    for entry in entries.flatten() {
      let fname = entry.file_name();
      if fname.to_string_lossy().to_lowercase() == "tera.exe" {
        return Some(entry.path());
      }
    }
  }
  None
}


#[tauri::command]
fn save_game_path_to_config(path: String) -> Result<(), String> {
  let config_path = find_config_file().ok_or("Config file not found")?;
  let mut conf = Ini::load_from_file(&config_path).map_err(|e|
    format!("Failed to load config: {}", e)
  )?;

  conf.with_section(Some("game")).set("path", &path);

  conf.write_to_file(&config_path).map_err(|e| format!("Failed to write config: {}", e))?;

  Ok(())
}

#[tauri::command]
fn get_game_path_from_config() -> Result<String, String> {
  match get_game_path() {
    Ok(game_path) => game_path
      .to_str()
      .ok_or_else(|| "Invalid UTF-8 in game path".to_string())
      .map(|s| s.to_string()),
    Err(e) => {
      if e.contains("Config file not found") {
        Err("config.ini is missing".to_string())
      } else {
        Err(e)
      }
    }
  }
}

#[tauri::command]
fn clear_update_cache() -> Result<(), String> {
  println!("Clearing update cache");
  let cache_path = get_cache_file_path()?;
  
  if cache_path.exists() {
    std::fs::remove_file(&cache_path)
      .map_err(|e| format!("Failed to delete cache file: {}", e))?;
    println!("Cache file deleted successfully");
    Ok(())
  } else {
    println!("Cache file does not exist");
    Ok(())
  }
}

#[tauri::command]
async fn check_update_required(window: tauri::Window) -> Result<bool, String> {
  match get_files_to_update(window).await {
    Ok(files) => Ok(!files.is_empty()),
    Err(e) => Err(e),
  }
}

// Security: Validate file paths to prevent path traversal attacks
fn is_safe_path(path: &str) -> bool {
  use std::path::Component;
  
  // Reject paths with parent directory references
  if path.contains("..") || path.starts_with("/") || path.starts_with("\\") {
    return false;
  }
  
  // Reject paths that normalize to a parent directory
  std::path::Path::new(path)
    .components()
    .all(|c| !matches!(c, Component::ParentDir | Component::RootDir))
}

#[tauri::command]
async fn update_file(
  _app_handle: tauri::AppHandle,
  window: tauri::Window,
  file_info: FileInfo,
  total_files: usize,
  current_file_index: usize,
  total_size: u64,
  downloaded_size: u64,
) -> Result<u64, String> {
  let game_path = get_game_path()?;
  
  // SECURITY: Validate file path to prevent path traversal attacks
  if !is_safe_path(&file_info.path) {
    return Err(format!("Invalid file path detected: {}. Path traversal attack blocked.", file_info.path));
  }
  
  let file_path = game_path.join(&file_info.path);
  
  // SECURITY: Ensure the final file path is within the game directory
  if !file_path.starts_with(&game_path) {
    return Err(format!("Path traversal attack detected. File would be extracted outside game directory."));
  }

  if let Some(parent) = file_path.parent() {
    tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
  }

  let client = reqwest::Client::builder()
    .no_proxy()
    .build()
    .map_err(|e| e.to_string())?;

  let res = client.get(&file_info.url)
    .send()
    .await
    .map_err(|e| e.to_string())?;

  let file_size = res.content_length().unwrap_or(file_info.size);
  let mut file = tokio::fs::File::create(&file_path).await.map_err(|e| e.to_string())?;
  let mut downloaded: u64 = 0;
  let mut stream = res.bytes_stream();
  let start_time = Instant::now();
  let mut last_update = Instant::now();

  println!("Downloading file: {}", file_info.path);

  while let Some(chunk_result) = stream.next().await {
    let chunk = chunk_result.map_err(|e| e.to_string())?;
    file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    downloaded += chunk.len() as u64;

    let now = Instant::now();
    if now.duration_since(last_update) >= Duration::from_millis(100) || downloaded == file_size {
      let elapsed = now.duration_since(start_time);
      let speed = if elapsed.as_secs() > 0 { downloaded / elapsed.as_secs() } else { downloaded };

      let total_downloaded = downloaded_size + downloaded;
      let progress_payload = ProgressPayload {
        file_name: file_info.path.clone(),
        progress: (downloaded as f64 / file_size as f64) * 100.0,
        speed: speed as f64,
        downloaded_bytes: total_downloaded,
        total_bytes: total_size,
        total_files,
        elapsed_time: elapsed.as_secs_f64(),
        current_file_index,
      };

      println!("Current file: {}, Download speed: {}/s, Progress: {:.2}%",
          file_info.path, format_bytes(speed), progress_payload.progress);

      if let Err(e) = window.emit("download_progress", &progress_payload) {
        println!("Failed to emit download_progress event: {}", e);
      }
      last_update = now;
    }

    tokio::time::sleep(Duration::from_millis(1)).await;
  }

  file.flush().await.map_err(|e| e.to_string())?;

  let downloaded_hash = tokio::task::spawn_blocking(move || calculate_file_hash(&file_path)).await.map_err(|e| e.to_string())??;
  if downloaded_hash != file_info.hash {
    return Err(format!("Hash mismatch for file: {}", file_info.path));
  }

  // Emit a final event for this file
  let final_progress_payload = ProgressPayload {
    file_name: file_info.path.clone(),
    progress: 100.0,
    speed: 0.0,
    downloaded_bytes: downloaded_size + downloaded,
    total_bytes: total_size,
    total_files,
    elapsed_time: start_time.elapsed().as_secs_f64(),
    current_file_index,
  };
  if let Err(e) = window.emit("download_progress", &final_progress_payload) {
    println!("Failed to emit final download_progress event: {}", e);
  }

  println!("File download completed: {}", file_info.path);

  Ok(downloaded)
}

fn format_bytes(bytes: u64) -> String {
  const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
  let mut size = bytes as f64;
  let mut unit_index = 0;

  while size >= 1024.0 && unit_index < UNITS.len() - 1 {
    size /= 1024.0;
    unit_index += 1;
  }

  format!("{:.2} {}", size, UNITS[unit_index])
}

#[tauri::command]
async fn download_all_files(
  app_handle: tauri::AppHandle,
  window: tauri::Window,
  files_to_update: Vec<FileInfo>
) -> Result<Vec<u64>, String> {
  let total_files = files_to_update.len();
  let total_size: u64 = files_to_update.iter().map(|f| f.size).sum();

  if total_files == 0 {
    println!("No files to download");
    if let Err(e) = window.emit("download_complete", ()) {
      eprintln!("Failed to emit download_complete event: {}", e);
    }
    return Ok(vec![]);
  }

  let mut downloaded_sizes = Vec::with_capacity(total_files);
  let mut downloaded_size: u64 = 0;

  for (index, file_info) in files_to_update.into_iter().enumerate() {
    let file_size = update_file(
      app_handle.clone(),
      window.clone(),
      file_info,
      total_files,
      index + 1,
      total_size,
      downloaded_size
    ).await?;

    downloaded_size += file_size;
    downloaded_sizes.push(file_size);
  }

  println!("Download complete for {} file(s)", total_files);
  if let Err(e) = window.emit("download_complete", ()) {
    eprintln!("Failed to emit download_complete event: {}", e);
  }

  Ok(downloaded_sizes)
}


#[tauri::command]
async fn get_files_to_update(window: tauri::Window) -> Result<Vec<FileInfo>, String> {
  println!("Starting get_files_to_update (normal - using cache)");

  let start_time = Instant::now();
  let server_hash_file = get_server_hash_file().await?;

  // Get the path to the game folder, which is the folder that contains the Tera game
  // files. This is the folder that we will be comparing with the server hash file
  // to determine which files need to be updated.
  let local_game_path = get_game_path()?;
  println!("Local game path: {:?}", local_game_path);

  println!("Attempting to read server hash file");
  let files = server_hash_file["files"].as_array().ok_or("Invalid server hash file format")?;
  println!("Server hash file parsed, {} files found", files.len());

  println!("Starting file comparison");
  let _cache = load_cache_from_disk().unwrap_or_else(|_| HashMap::new());
  let cache = Arc::new(RwLock::new(_cache));

  let progress_bar = ProgressBar::new(files.len() as u64);
  progress_bar.set_style(ProgressStyle::default_bar()
    .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
    .unwrap()
    .progress_chars("##-"));

  let processed_count = Arc::new(AtomicUsize::new(0));
  let files_to_update_count = Arc::new(AtomicUsize::new(0));
  let total_size = Arc::new(AtomicU64::new(0));

  let files_to_update: Vec<FileInfo> = files.par_iter().enumerate()
    .filter_map(|(_index, file_info)| {
      let path = file_info["path"].as_str().unwrap_or("");
      let server_hash = file_info["hash"].as_str().unwrap_or("");
      let size = file_info["size"].as_u64().unwrap_or(0);
      let url = file_info["url"].as_str().unwrap_or("").to_string();

      let local_file_path = local_game_path.join(path);

      let current_count = processed_count.fetch_add(1, Ordering::SeqCst) + 1;
      if current_count % 100 == 0 || current_count == files.len() {
        let progress_payload = FileCheckProgress {
          current_file: path.to_string(),
          progress: (current_count as f64 / files.len() as f64) * 100.0,
          current_count,
          total_files: files.len(),
          elapsed_time: start_time.elapsed().as_secs_f64(),
          files_to_update: files_to_update_count.load(Ordering::SeqCst),
        };

        let _ = window.emit("file_check_progress", progress_payload)
          .map_err(|e| {
            println!("Error emitting file_check_progress event: {}", e);
            e.to_string()
          });
      }

      progress_bar.inc(1);

      if !local_file_path.exists() {
        files_to_update_count.fetch_add(1, Ordering::SeqCst);
        total_size.fetch_add(size, Ordering::SeqCst);
        return Some(FileInfo {
          path: path.to_string(),
          hash: server_hash.to_string(),
          size,
          url,
        });
      }

      let metadata = match fs::metadata(&local_file_path) {
        Ok(m) => m,
        Err(_) => {
          files_to_update_count.fetch_add(1, Ordering::SeqCst);
          total_size.fetch_add(size, Ordering::SeqCst);
          return Some(FileInfo {
            path: path.to_string(),
            hash: server_hash.to_string(),
            size,
            url,
          });
        }
      };

      let last_modified = metadata.modified().ok();

      let cache_read = cache.read().unwrap();
      if let Some(cached_info) = cache_read.get(path) {
        if let Some(lm) = last_modified {
          if cached_info.last_modified == lm && cached_info.hash == server_hash {
            return None;
          }
        }
      }
      drop(cache_read);

      if metadata.len() != size {
        files_to_update_count.fetch_add(1, Ordering::SeqCst);
        total_size.fetch_add(size, Ordering::SeqCst);
        return Some(FileInfo {
          path: path.to_string(),
          hash: server_hash.to_string(),
          size,
          url,
        });
      }

      let local_hash = match calculate_file_hash(&local_file_path) {
        Ok(hash) => hash,
        Err(_) => {
          files_to_update_count.fetch_add(1, Ordering::SeqCst);
          total_size.fetch_add(size, Ordering::SeqCst);
          return Some(FileInfo {
            path: path.to_string(),
            hash: server_hash.to_string(),
            size,
            url,
          });
        }
      };

      let mut cache_write = cache.write().unwrap();
      cache_write.insert(path.to_string(), CachedFileInfo {
        hash: local_hash.clone(),
        last_modified: last_modified.unwrap_or_else(SystemTime::now),
      });
      drop(cache_write);

      if local_hash != server_hash {
        files_to_update_count.fetch_add(1, Ordering::SeqCst);
        total_size.fetch_add(size, Ordering::SeqCst);
        Some(FileInfo {
          path: path.to_string(),
          hash: server_hash.to_string(),
          size,
          url,
        })
      } else {
        None
      }
    })
    .collect();

  progress_bar.finish_with_message("File comparison completed");

  // Save the updated cache to disk
  let final_cache = cache.read().unwrap();
  if let Err(e) = save_cache_to_disk(&*final_cache) {
    eprintln!("Failed to save cache to disk: {}", e);
  }

  let total_time = start_time.elapsed();
  println!("File comparison completed. Files to update: {}", files_to_update.len());

  // Emit a final event with complete statistics
  let _ = window.emit("file_check_completed", json!({
    "total_files": files.len(),
    "files_to_update": files_to_update.len(),
    "total_size": total_size.load(Ordering::SeqCst),
    "total_time_seconds": total_time.as_secs(),
    "average_time_per_file_ms": (total_time.as_millis() as f64) / (files.len() as f64)
  }));

  Ok(files_to_update)
}

#[tauri::command]
async fn get_files_to_update_force(window: tauri::Window) -> Result<Vec<FileInfo>, String> {
  println!("Starting get_files_to_update_force (FORCE MODE - ignoring cache)");

  let start_time = Instant::now();
  let server_hash_file = get_server_hash_file().await?;

  // Get the path to the game folder, which is the folder that contains the Tera game
  // files. This is the folder that we will be comparing with the server hash file
  // to determine which files need to be updated.
  let local_game_path = get_game_path()?;
  println!("Local game path: {:?}", local_game_path);

  println!("Attempting to read server hash file");
  let files = server_hash_file["files"].as_array().ok_or("Invalid server hash file format")?;
  println!("Server hash file parsed, {} files found", files.len());

  println!("Starting file comparison (FORCE MODE - empty cache)");
  // In force mode, we use an empty cache so all files are rechecked
  let _cache: HashMap<String, CachedFileInfo> = HashMap::new();
  let cache = Arc::new(RwLock::new(_cache));

  let progress_bar = ProgressBar::new(files.len() as u64);
  progress_bar.set_style(ProgressStyle::default_bar()
    .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
    .unwrap()
    .progress_chars("##-"));

  let processed_count = Arc::new(AtomicUsize::new(0));
  let files_to_update_count = Arc::new(AtomicUsize::new(0));
  let total_size = Arc::new(AtomicU64::new(0));

  let files_to_update: Vec<FileInfo> = files.par_iter().enumerate()
    .filter_map(|(_index, file_info)| {
      let path = file_info["path"].as_str().unwrap_or("");
      let server_hash = file_info["hash"].as_str().unwrap_or("");
      let size = file_info["size"].as_u64().unwrap_or(0);
      let url = file_info["url"].as_str().unwrap_or("").to_string();

      let local_file_path = local_game_path.join(path);

      let current_count = processed_count.fetch_add(1, Ordering::SeqCst) + 1;
      if current_count % 100 == 0 || current_count == files.len() {
        let progress_payload = FileCheckProgress {
          current_file: path.to_string(),
          progress: (current_count as f64 / files.len() as f64) * 100.0,
          current_count,
          total_files: files.len(),
          elapsed_time: start_time.elapsed().as_secs_f64(),
          files_to_update: files_to_update_count.load(Ordering::SeqCst),
        };

        let _ = window.emit("file_check_progress", progress_payload)
          .map_err(|e| {
            println!("Error emitting file_check_progress event: {}", e);
            e.to_string()
          });
      }

      progress_bar.inc(1);

      if !local_file_path.exists() {
        files_to_update_count.fetch_add(1, Ordering::SeqCst);
        total_size.fetch_add(size, Ordering::SeqCst);
        return Some(FileInfo {
          path: path.to_string(),
          hash: server_hash.to_string(),
          size,
          url,
        });
      }

      let metadata = match fs::metadata(&local_file_path) {
        Ok(m) => m,
        Err(_) => {
          files_to_update_count.fetch_add(1, Ordering::SeqCst);
          total_size.fetch_add(size, Ordering::SeqCst);
          return Some(FileInfo {
            path: path.to_string(),
            hash: server_hash.to_string(),
            size,
            url,
          });
        }
      };

      let local_hash = match calculate_file_hash(&local_file_path) {
        Ok(hash) => hash,
        Err(_) => {
          files_to_update_count.fetch_add(1, Ordering::SeqCst);
          total_size.fetch_add(size, Ordering::SeqCst);
          return Some(FileInfo {
            path: path.to_string(),
            hash: server_hash.to_string(),
            size,
            url,
          });
        }
      };

      // In force mode, even if hashes match, we mark the file as needing update
      // to force a complete rebuild of the cache
      if server_hash == local_hash {
        // Update cache with current file info (this rebuilds the cache)
        cache.write().unwrap().insert(path.to_string(), CachedFileInfo {
          hash: server_hash.to_string(),
          last_modified: metadata.modified().unwrap_or(SystemTime::now()),
        });
        None
      } else {
        files_to_update_count.fetch_add(1, Ordering::SeqCst);
        total_size.fetch_add(size, Ordering::SeqCst);
        Some(FileInfo {
          path: path.to_string(),
          hash: server_hash.to_string(),
          size,
          url,
        })
      }
    })
    .collect();

  progress_bar.finish_with_message("File comparison completed");

  // Save the rebuilt cache
  let cache_data = cache.read().unwrap().clone();
  let _ = save_cache_to_disk(&cache_data);

  println!("File comparison completed. Files to update: {}", files_to_update.len());

  let total_time = start_time.elapsed();
  window.emit("file_check_completed", json!({
    "success": true,
    "total_files": files.len(),
    "files_to_update": files_to_update.len(),
    "total_size": total_size.load(Ordering::SeqCst),
    "elapsed_time": total_time.as_secs_f64(),
    "average_time_per_file_ms": (total_time.as_millis() as f64) / (files.len() as f64)
  })).ok();

  Ok(files_to_update)
}


#[tauri::command]
async fn get_game_status(state: tauri::State<'_, GameState>) -> Result<bool, String> {
  let status = state.status_receiver.lock().await.borrow().clone();
  let is_launching = *state.is_launching.lock().await;
  Ok(status || is_launching)
}

#[tauri::command]
async fn handle_launch_game(
  app_handle: tauri::AppHandle,
  state: tauri::State<'_, GameState>
) -> Result<String, String> {
  println!("handle_launch_game: Starting");
  
  // Step 1: Check if game is already launching or running
  let mut is_launching = state.is_launching.lock().await;
  if *is_launching {
    println!("handle_launch_game: Already launching");
    return Err("Game is already launching".to_string());
  }
  *is_launching = true;

  let is_running = *state.status_receiver.lock().await.borrow();
  if is_running {
    println!("handle_launch_game: Game already running");
    *is_launching = false;
    return Err("Game is already running".to_string());
  }

  // Step 2: Validate and retrieve authentication info
  println!("handle_launch_game: Validating authentication info");
  let (account_name, characters_count, ticket) = {
    let auth_info = GLOBAL_AUTH_INFO.read()
      .map_err(|e| {
        *is_launching = false;
        format!("Failed to read auth info: {}", e)
      })?;
    
    // Validate all required fields are present and non-empty
    if auth_info.auth_key.is_empty() {
      *is_launching = false;
      return Err("Auth key is missing. Please login again.".to_string());
    }
    
    if auth_info.user_no <= 0 {
      *is_launching = false;
      return Err("Invalid user number. Please login again.".to_string());
    }
    
    if auth_info.character_count.is_empty() {
      *is_launching = false;
      return Err("Character count is missing. Please login again.".to_string());
    }
    
    println!("handle_launch_game: Auth validation successful for user_no: {}", auth_info.user_no);
    (
      auth_info.user_no.to_string(),
      auth_info.character_count.clone(),
      auth_info.auth_key.clone()
    )
  };

  // Step 3: Load and validate game configuration
  let (game_path, game_lang) = match load_config() {
    Ok(config) => config,
    Err(e) => {
      *is_launching = false;
      return Err(format!("Failed to load game config: {}", e));
    }
  };
  
  println!("handle_launch_game: Game path: {:?}, lang: {}", game_path, game_lang);
  
  if !game_path.exists() {
    *is_launching = false;
    return Err(format!("Game path does not exist: {:?}", game_path));
  }

  // Step 4: Validate game executable exists (case-insensitive on Linux)
  let full_game_path = find_game_exe(&game_path.join("Binaries"))
    .ok_or_else(|| {
      *is_launching = false;
      format!("Game executable not found in {:?}/Binaries. Please verify your game installation.", game_path)
    });
  let full_game_path = match full_game_path {
    Ok(p) => p,
    Err(e) => { *is_launching = false; return Err(e); }
  };

  let full_game_path_str = match full_game_path.to_str() {
    Some(path_str) => path_str.to_string(),
    None => {
      *is_launching = false;
      return Err("Invalid characters in game executable path".to_string());
    }
  };

  // Step 5: Retrieve and validate ACTS_MAP and PAGES_MAP
  let (acts_map_clone, pages_map_clone) = {
    let acts_map_guard = GLOBAL_ACTS_MAP.read()
      .map_err(|e| {
        *is_launching = false;
        format!("Failed to read ACTS_MAP: {}", e)
      })?;
    
    let pages_map_guard = GLOBAL_PAGES_MAP.read()
      .map_err(|e| {
        *is_launching = false;
        format!("Failed to read PAGES_MAP: {}", e)
      })?;
    
    if acts_map_guard.is_empty() {
      println!("Warning: ACTS_MAP is empty");
    }
    
    if pages_map_guard.is_empty() {
      println!("Warning: PAGES_MAP is empty");
    }
    
    println!("handle_launch_game: ACTS_MAP entries: {}, PAGES_MAP entries: {}", 
      acts_map_guard.len(), 
      pages_map_guard.len()
    );
    
    (acts_map_guard.clone(), pages_map_guard.clone())
  };

  // Step 6: Spawn the game launch in background
  let app_handle_clone = app_handle.clone();
  let is_launching_clone = Arc::clone(&state.is_launching);

  tokio::task::spawn(async move {
    // Emit the game_status_changed event at the start of the launch
    if let Err(e) = app_handle_clone.emit_all("game_status_changed", true) {
      error!("Failed to emit game_status_changed event: {:?}", e);
    }

    info!("Launching game with executable: {}", full_game_path_str);
    let launch_error: Option<String>;
    match
      run_game(
        &account_name,
        &characters_count,
        &ticket,
        &game_lang,
        &full_game_path_str,
        acts_map_clone,
        pages_map_clone,
      ).await
    {
      Ok(exit_status) => {
        let result = format!("Game exited with status: {:?}", exit_status);
        app_handle_clone.emit_all("game_status", &result).unwrap();
        info!("{}", result);
        launch_error = None;
      }
      Err(e) => {
        let error = format!("Error launching game: {:?}", e);
        app_handle_clone.emit_all("game_status", &error).unwrap();
        error!("{}", error);
        launch_error = Some(e.to_string());
      }
    }

    // Emit structured exit info (code + reason) so the frontend can show a message.
    {
      let exit_info = get_last_exit_info();
      let crash_details = get_last_crash_details();
      let stderr = get_last_game_stderr();
      let payload = serde_json::json!({
        "code":   exit_info.code,
        "reason": exit_info.reason,
        "crash":  !crash_details.is_empty(),
        "details": crash_details,
        "stderr": stderr,
        // non-null when run_game() itself errored (not a TERA exit code)
        "launch_error": launch_error,
      });
      if let Err(e) = app_handle_clone.emit_all("game_exit_info", payload) {
        error!("Failed to emit game_exit_info event: {:?}", e);
      }
    }

    info!("Emitting game_ended event");
    if let Err(e) = app_handle_clone.emit_all("game_ended", ()) {
      error!("Failed to emit game_ended event: {:?}", e);
    }

    let mut is_launching = is_launching_clone.lock().await;
    *is_launching = false;
    if let Err(e) = app_handle_clone.emit_all("game_status_changed", false) {
      error!("Failed to emit game_status_changed event: {:?}", e);
    }

    reset_global_state();

    info!("Game launch state reset");
  });

  Ok("Game launch initiated".to_string())
}


#[tauri::command]
fn get_language_from_config() -> Result<String, String> {
  info!("Attempting to read language from config file");
  let (_, game_lang) = load_config()?;
  info!("Language read from config: {}", game_lang);
  Ok(game_lang)
}

#[tauri::command]
fn save_language_to_config(language: String) -> Result<(), String> {
  info!("Attempting to save language {} to config file", language);
  let config_path = find_config_file().ok_or("Config file not found")?;
  let mut conf = Ini::load_from_file(&config_path).map_err(|e|
    format!("Failed to load config: {}", e)
  )?;

  conf.with_section(Some("game")).set("lang", &language);

  conf.write_to_file(&config_path).map_err(|e| format!("Failed to write config: {}", e))?;

  info!("Language successfully saved to config");
  Ok(())
}

#[tauri::command]
async fn reset_launch_state(state: tauri::State<'_, GameState>) -> Result<(), String> {
  let mut is_launching = state.is_launching.lock().await;
  *is_launching = false;
  Ok(())
}

#[tauri::command]
async fn set_auth_info( 
  auth_key: String, 
  user_name: String, 
  user_no: i32, 
  character_count: String,
  session_cookie: Option<String>, 
) { 
  {
    let mut auth_info = GLOBAL_AUTH_INFO.write().unwrap();
    auth_info.auth_key = auth_key;
    auth_info.user_name = user_name;
    auth_info.user_no = user_no;
    auth_info.character_count = character_count;

    info!("Auth info set from frontend:");
    info!("User Name: {}", auth_info.user_name);
    info!("User No: {}", auth_info.user_no);
    info!("Character Count: {}", auth_info.character_count);
    info!("Auth Key: {}", auth_info.auth_key);
  }

  if let Some(cookie_value) = session_cookie {
    if !cookie_value.is_empty() {
      info!("Rebuilding authenticated client from stored cookie...");
      let base_url = &*LAUNCHER_BASE_URL;
      let url = Url::parse(base_url).expect("Failed to parse LAUNCHER_BASE_URL");
      let host = url.host_str().expect("LAUNCHER_BASE_URL has no host");

      // Build cookie
      let cookie_str = format!("launcher.sid={}; Domain={}; Path=/", cookie_value, host);
      
      let jar = Arc::new(Jar::default());
      jar.add_cookie_str(&cookie_str, &url);

      // Build new client using the cookie jar
      let client = Client::builder()
        .cookie_store(true)
        .cookie_provider(jar)
        .build()
        .expect("Failed to rebuild client");

      // Store client globally
      let mut client_guard = AUTHENTICATED_CLIENT.lock().await; // <-- 6. 'await' is now valid
      *client_guard = Some(client);
      info!("Authenticated client rebuilt successfully.");
    } else {
      info!("No session cookie found to rebuild client.");
    }
  }
}


/// Handles the complete login process for the TERA launcher.
///
/// ### Overview
/// This function:
/// 1. Authenticates the user using their credentials.
/// 2. Retrieves the session cookie and essential account details (account info, auth key, character count).
/// 3. Fetches and parses the main launcher HTML page to extract `ACTS_MAP` and `PAGES_MAP`.
/// 4. Stores these maps globally for future use.
/// 5. Returns a structured JSON response with all relevant login and session data.
///
/// The function communicates with the launcher’s backend endpoints, maintains cookies
/// across requests, and reconstructs necessary URLs dynamically using `LAUNCHER_BASE_URL`.
///
/// ### Arguments
/// * `username` - The user's login name.
/// * `password` - The user's password.
///
/// ### Returns
/// * `Ok(String)` - JSON containing authentication results and user data.
/// * `Err(String)` - A descriptive error message in case of failure.
#[tauri::command]
async fn login(username: String, password: String) -> Result<String, String> {
    // 1. Create an HTTP client with a persistent cookie jar
    let cookie_jar = Arc::new(Jar::default());
    let client = Client::builder()
        .cookie_store(true)
        .cookie_provider(Arc::clone(&cookie_jar))
        .build()
        .map_err(|e| e.to_string())?;

    // --- Step 1: Define Base URL ---
    // The base launcher URL is obtained once from the global constant.
    let base_url = &*LAUNCHER_BASE_URL;
    let login_url = format!("{}/launcher/LoginAction", base_url);

    // --- Step 2: POST to /launcher/LoginAction (Authentication) ---
    // Use .form() to properly encode parameters and prevent URL injection attacks
    let login_res = client
        .post(&login_url)
        .form(&[("login", username.as_str()), ("password", password.as_str())])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    // --- Parse the login response and extract session cookie ---
    if !login_res.status().is_success() {
        return Err(format!("Login request failed with status: {}", login_res.status()));
    }

    let login_body: InitialLoginResponse = login_res
        .json()
        .await
        .map_err(|e| format!("Failed to parse login response: {}.", e))?;

    if !login_body.return_value {
        return Err(login_body.msg);
    }

    // Parse the cookies to retrieve the session identifier (launcher.sid)
    let login_url_parsed = Url::parse(&login_url)
        .map_err(|e| format!("Failed to parse login URL: {}", e))?;
    let cookie_header_value = cookie_jar.cookies(&login_url_parsed);

    let session_cookie: Option<String> = cookie_header_value
        .and_then(|header_val| header_val.to_str().ok().map(String::from))
        .and_then(|cookie_str| {
            cookie_str.split(';').find_map(|cookie_pair| {
                let cookie_pair = cookie_pair.trim();
                if cookie_pair.starts_with("launcher.sid=") {
                    Some(cookie_pair.trim_start_matches("launcher.sid=").to_string())
                } else {
                    None
                }
            })
        });

    let success_msg = login_body.msg.clone();

    // --- Step 3: Retrieve account data using the authenticated client ---
    // These endpoints depend on the valid session cookie.
    let account_info_url = format!("{}/launcher/GetAccountInfoAction", base_url);
    let account_info: AccountInfoResponse = client
        .get(&account_info_url)
        .send()
        .await
        .map_err(|e| format!("Failed to get account info: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse account info: {}", e))?;

    let auth_key_url = format!("{}/launcher/GetAuthKeyAction", base_url);
    let auth_key: AuthKeyResponse = client
        .get(&auth_key_url)
        .send()
        .await
        .map_err(|e| format!("Failed to get auth key: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse auth key: {}", e))?;

    let char_count_url = format!("{}/launcher/GetCharacterCountAction", base_url);
    let char_count: CharCountResponse = client
        .get(&char_count_url)
        .send()
        .await
        .map_err(|e| format!("Failed to get character count: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse character count: {}", e))?;

    // --- Step 4: GET /launcher/Main to extract ActsMap/PagesMap ---
    // Important: Add locale if the server requires it to serve localized pages.
    let main_url = format!("{}/launcher/Main?locale=en", base_url);

    let main_res = client
        .get(&main_url)
        .send()
        .await
        .map_err(|e| format!("Failed to get launcher main page: {}", e))?;

    if !main_res.status().is_success() {
        return Err(format!(
            "Launcher main page request failed with status: {}",
            main_res.status()
        ));
    }

    let main_html = main_res
        .text()
        .await
        .map_err(|e| format!("Failed to read main page body: {}", e))?;

    // KEY STEP: Pass `base_url` so that `extract_maps_from_html` can rebuild the full URLs in ACTS_MAP.
    let (acts_map, pages_map) = extract_maps_from_html(&main_html, base_url)?;

    // --- Step 5: Save parsed maps into global state ---
    if let Some(map_object) = acts_map.as_object() {
        let mut acts_map_guard = GLOBAL_ACTS_MAP.write().unwrap();
        acts_map_guard.clear();
        for (key, value) in map_object {
            if let Some(url_str) = value.as_str() {
                acts_map_guard.insert(key.clone(), url_str.to_string());
            }
        }
        info!("Saved GLOBAL_ACTS_MAP with {} entries", acts_map_guard.len());
    }

    if let Some(map_object) = pages_map.as_object() {
        let mut pages_map_guard = GLOBAL_PAGES_MAP.write().unwrap();
        pages_map_guard.clear(); // Clear previous map before inserting new values
        for (key, value) in map_object {
            if let Some(url_str) = value.as_str() {
                pages_map_guard.insert(key.clone(), url_str.to_string());
            }
        }
        info!("Saved GLOBAL_PAGES_MAP with {} entries", pages_map_guard.len());
    }

    // --- Step 6: Consolidate and return the final JSON response ---
    let combined_response = CombinedLoginResponse {
        return_value: true,
        return_code: login_body.return_code,
        msg: success_msg,
        character_count: char_count.character_count,
        permission: account_info.permission,
        privilege: account_info.privilege,
        user_no: account_info.user_no,
        user_name: account_info.user_name,
        auth_key: auth_key.auth_key,
        banned: account_info.banned,

        acts_map: Some(acts_map),
        pages_map: Some(pages_map),

        session_cookie: session_cookie,
    };

    // Store the authenticated client globally for subsequent API calls
    let mut client_guard = AUTHENTICATED_CLIENT.lock().await;
    *client_guard = Some(client);

    // Serialize and return the combined response as JSON
    serde_json::to_string(&combined_response)
        .map_err(|e| format!("Failed to serialize final login response: {}", e))
}

#[tauri::command]
async fn handle_logout(state: tauri::State<'_, GameState>) -> Result<(), String> {
  let mut is_launching = state.is_launching.lock().await;
  *is_launching = false;

  // Step 1: Attempt to revoke the session on the server (security best practice)
  // This ensures the auth_key is invalidated server-side, even if compromised
  let auth_key = {
    let auth_info = GLOBAL_AUTH_INFO.read().unwrap();
    auth_info.auth_key.clone()
  };  // Lock is released here before the await point

  if !auth_key.is_empty() {
    let base_url = &*LAUNCHER_BASE_URL;
    let logout_url = format!("{}/launcher/LogoutAction", base_url);
    
    if let Ok(_response) = reqwest::Client::new().get(&logout_url).send().await {
      info!("Server logout completed");
    } else {
      error!("Failed to revoke session on server, proceeding with local logout");
    }
  }

  // Step 2: Reset global authentication information locally
  {
    let mut auth_info = GLOBAL_AUTH_INFO.write().unwrap();
    auth_info.auth_key = String::new();
    auth_info.user_name = String::new();
    auth_info.user_no = 0;
    auth_info.character_count = String::new();
  }

  {
    let mut pages_map = GLOBAL_PAGES_MAP.write().unwrap();
    pages_map.clear();
    info!("GLOBAL_PAGES_MAP cleared.");
  }

  {
    let mut pages_map = GLOBAL_ACTS_MAP.write().unwrap();
    pages_map.clear();
    info!("GLOBAL_ACTS_MAP cleared.");
  }

  let mut client_guard = AUTHENTICATED_CLIENT.lock().await;
  *client_guard = None;

  Ok(())
}

// Modification: We need to access LAUNCHER_BASE_URL inside this function,
// but it’s not a parameter. The solution is to pass it as an argument to the function,
// and update the call in `login` accordingly.
// Move this line if it’s not already at the top of the file.
// use regex::Regex;
fn extract_maps_from_html(
    html: &str,
    base_url: &str
) -> Result<(serde_json::Value, serde_json::Value), String> {
    use regex::Regex;
    
    lazy_static! {
        // Expression to extract the full block of the ACTS_MAP/PAGES_MAP variable (including the braces).
        static ref RE_ACTSMAP: Regex = 
            Regex::new(r"var ACTS_MAP\s*=\s*(\{[\s\S]*?\});").expect("Invalid actsMap regex");
            
        static ref RE_PAGESMAP: Regex = 
            Regex::new(r"var PAGES_MAP\s*=\s*(\{[\s\S]*?\});").expect("Invalid pagesMap regex");

        // KEY FIX FOR ACTS_MAP: Captures the Key (Group 1) and the PATH (Group 2)
        // Pattern looks for: Key: location.protocol + "//HOST:PORT/PATH"
        // G1 (\w+): The numeric key (e.g., 210)
        // G2 (/[^"]*?): The path starting with '/' and ending before the closing quote.
        static ref RE_ACTS_ITEM_PATH: Regex = 
            Regex::new(r#"(\w+):\s*location\.protocol\s*\+\s*"//\S+?(/[^"]*?)",?"#).expect("Invalid actsMap item path regex");


        // --- Regex for PAGES_MAP (General cleanup) ---
        
        // Quote all unquoted keys that are tokens (word or number).
        static ref RE_QUOTE_UNQUOTED_KEYS: Regex = 
            Regex::new(r#"([,\s{])(\w+)(\s*?:)"#).expect("Invalid quote unquoted keys regex");
        
        // Replace JS expressions (if any) with a valid string for PAGES_MAP.
        static ref RE_JS_PROTOCOL_PAGESMAP: Regex = 
            Regex::new(r"(location\.protocol[\s\S]+?)(\}|,)").expect("Invalid pagesMap JS value regex");
            
        // Remove trailing comma.
        static ref RE_TRAILING_COMMA: Regex = 
            Regex::new(r",\s*?\}").expect("Invalid trailing comma regex");
            
        // Whitespace normalization
        static ref RE_NORMALIZE_WHITESPACE: Regex = 
            Regex::new(r"[\r\n\t ]+").expect("Invalid normalize whitespace regex");
    }

    // 1. EXTRACT AND BUILD ACTS_MAP (Manual URL reconstruction)

    let acts_map_raw = RE_ACTSMAP.captures(html)
        .and_then(|cap| cap.get(1))
        .map(|m| m.as_str())
        .ok_or("Could not find ACTS_MAP in HTML")?;
    
    eprintln!("DEBUG: ACTS_MAP Raw Content:\n{}", acts_map_raw); // <- DEBUG 1

    let mut final_acts_map = serde_json::Map::new();

    // Iterate over all (Key: Path) matches in the raw string
    for cap in RE_ACTS_ITEM_PATH.captures_iter(acts_map_raw) {
        // Group 1: Key (e.g., "210")
        // Group 2: Path (e.g., "/tera/ShopAuth?authKey=%s")
        let key = cap.get(1).unwrap().as_str().to_string();
        let path = cap.get(2).unwrap().as_str(); // This path is already just the route
        
        // Rebuild the URL: base_url + path
        let final_url = format!("{}{}", base_url, path);
        
        final_acts_map.insert(key, serde_json::Value::String(final_url));
    }
    
    let acts_map = serde_json::Value::Object(final_acts_map);
    eprintln!("DEBUG: ACTS_MAP Final JSON:\n{}", acts_map.to_string()); // <- DEBUG 2


    // 2. EXTRACT AND CLEAN PAGES_MAP (String cleanup)
    let pages_map_raw = RE_PAGESMAP.captures(html)
        .and_then(|cap| cap.get(1))
        .map(|m| m.as_str())
        .ok_or("Could not find PAGES_MAP in HTML")?;
    
    let mut pages_map_str = pages_map_raw.to_string();
    
    // Apply cleanup: normalization, JS value replacement, quoting keys, trailing comma removal.
    pages_map_str = RE_NORMALIZE_WHITESPACE.replace_all(&pages_map_str, " ").to_string();
    pages_map_str = pages_map_str.trim().replace("{ ", "{").replace(" }", "}");
    
    // Replace JS expressions (Group 1) with a placeholder string (if any exist in PAGES_MAP)
    pages_map_str = RE_JS_PROTOCOL_PAGESMAP.replace_all(&pages_map_str, r#""/JS/EXPRESSION/REMOVED"$2"#).to_string();

    // Quote all unquoted keys.
    pages_map_str = RE_QUOTE_UNQUOTED_KEYS.replace_all(&pages_map_str, r#"$1"$2"$3"#).to_string();

    // Remove trailing comma.
    pages_map_str = RE_TRAILING_COMMA.replace_all(&pages_map_str, "}").to_string();

    eprintln!("DEBUG: PAGES_MAP Cleaned Content Final:\n{}", pages_map_str); // <- DEBUG 3

    // 3. PARSE PAGES_MAP
    let pages_map: serde_json::Value = serde_json::from_str(&pages_map_str)
        .map_err(|e| format!("Failed to parse pagesMap JSON: {}", e))?;

    Ok((acts_map, pages_map))
}

#[tauri::command]
async fn check_server_connection() -> Result<bool, String> {
  // Step 1: Validate that we have an authenticated session with complete credentials
  let auth_valid = {
    let auth_info = GLOBAL_AUTH_INFO.read()
      .map_err(|e| format!("Failed to read auth info: {}", e))?;
    
    let is_complete = 
      !auth_info.auth_key.is_empty() &&
      auth_info.user_no > 0 &&
      !auth_info.user_name.is_empty() &&
      !auth_info.character_count.is_empty();
    
    if !is_complete {
      println!("Auth info incomplete: auth_key={}, user_no={}, user_name={}, char_count={}", 
        !auth_info.auth_key.is_empty(),
        auth_info.user_no,
        !auth_info.user_name.is_empty(),
        !auth_info.character_count.is_empty()
      );
      return Err("Authentication is incomplete. Please login again.".to_string());
    }
    
    println!("Auth info valid and complete for user: {}", auth_info.user_name);
    true
  };
  
  if !auth_valid {
    return Err("Authentication validation failed".to_string());
  }
  
  // Step 2: Verify we have an authenticated HTTP client
  let client_guard = AUTHENTICATED_CLIENT.lock().await;
  let client = match &*client_guard {
    Some(client) => client,
    None => {
      println!("No authenticated HTTP client available");
      return Err("No authenticated session found. Please login again.".to_string());
    }
  };
  
  // Step 3: Attempt actual connection to server using a simple request
  let hash_file_url = get_hash_file_url();
  println!("Attempting server connection to: {}", hash_file_url);
  
  match client.get(&hash_file_url).send().await {
    Ok(response) => {
      let status = response.status();
      println!("Server connection check: status {}", status);
      
      if status.is_success() {
        Ok(true)
      } else if status.is_client_error() {
        // 4xx errors might indicate auth issues
        Err(format!("Server returned client error: {}", status))
      } else if status.is_server_error() {
        // 5xx errors indicate server issues
        Err(format!("Server returned server error: {}", status))
      } else {
        Err(format!("Unexpected server response: {}", status))
      }
    },
    Err(e) => {
      println!("Server connection check failed: {}", e);
      Err(format!("Failed to connect to server: {}", e))
    }
  }
}

#[tauri::command]
fn get_client_version() -> Result<String, String> {
  Ok(get_config_value("CLIENT_VERSION"))
}

#[tauri::command]
async fn get_fresh_account_info() -> Result<String, String> {
  let client_guard = AUTHENTICATED_CLIENT.lock().await;
  let client = match &*client_guard {
    Some(client) => {
      println!("get_fresh_account_info: Authenticated client found");
      client
    },
    None => {
      println!("get_fresh_account_info: No authenticated client available");
      return Err("User session expired. Please login again.".to_string());
    }
  };
  
  let base_url = &*LAUNCHER_BASE_URL;
  
  // Step 2: Fetch account info from server
  println!("get_fresh_account_info: Fetching account info");
  let account_info_url = format!("{}/launcher/GetAccountInfoAction", base_url);
  let account_info: AccountInfoResponse = client
    .get(&account_info_url)
    .send()
    .await
    .map_err(|e| format!("Failed to connect to account info endpoint: {}", e))?
    .json()
    .await
    .map_err(|e| format!("Failed to parse account info response: {}", e))?;
  
  if account_info.user_no <= 0 {
    return Err("Invalid user number received from server".to_string());
  }
  println!("get_fresh_account_info: Account info retrieved for user_no: {}", account_info.user_no);

    // --- Step 2: GET /launcher/GetAuthKeyAction ---
    // Request a new AuthKey so that the Node.js backend knows we’re still active
    // and to ensure we’re using a valid key.
    let auth_key_url = format!("{}/launcher/GetAuthKeyAction", base_url);
    let auth_key: AuthKeyResponse = client
      .get(&auth_key_url)
      .send()
      .await
      .map_err(|e| format!("(Re-check) Failed to get auth key: {}", e))?
      .json()
      .await
      .map_err(|e| format!("(Re-check) Failed to parse auth key: {}", e))?;

    // --- Step 3: GET /launcher/GetCharacterCountAction ---
    let char_count_url = format!("{}/launcher/GetCharacterCountAction", base_url);
    let char_count: CharCountResponse = client
      .get(&char_count_url)
      .send()
      .await
      .map_err(|e| format!("Failed to connect to character count endpoint: {}", e))?
      .json()
      .await
      .map_err(|e| format!("Failed to parse character count response: {}", e))?;
    
    if char_count.character_count.is_empty() {
      return Err("Server returned empty character count".to_string());
    }

    // actsMap & pagesMap refresh
    println!("get_fresh_account_info: Refreshing ACTS_MAP and PAGES_MAP");
    let main_url = format!("{}/launcher/Main?locale=en", base_url);  
    
    let main_res = client
      .get(&main_url)
      .send()
      .await
      .map_err(|e| format!("Failed to fetch launcher main page: {}", e))?;

    if !main_res.status().is_success() {
      return Err(format!("Launcher main page returned error status: {}", main_res.status()));
    }

    let main_html = main_res.text().await.map_err(|e| format!("Failed to read launcher main page: {}", e))?;
    let (acts_map, pages_map) = extract_maps_from_html(&main_html, base_url)?;

    if let Some(map_object) = pages_map.as_object() {
        let mut pages_map_guard = GLOBAL_PAGES_MAP.write()
          .map_err(|e| format!("Failed to write PAGES_MAP: {}", e))?;
        pages_map_guard.clear();
        for (key, value) in map_object {
            if let Some(url_str) = value.as_str() {
                pages_map_guard.insert(key.clone(), url_str.to_string());
            }
        }
        println!("get_fresh_account_info: PAGES_MAP updated with {} entries", pages_map_guard.len());
    } else {
      return Err("PAGES_MAP is not a valid object".to_string());
    }

    if let Some(map_object) = acts_map.as_object() {
        let mut acts_map_guard = GLOBAL_ACTS_MAP.write()
          .map_err(|e| format!("Failed to write ACTS_MAP: {}", e))?;
        acts_map_guard.clear();
        for (key, value) in map_object {
            if let Some(url_str) = value.as_str() {
                acts_map_guard.insert(key.clone(), url_str.to_string());
            }
        }
        println!("get_fresh_account_info: ACTS_MAP updated with {} entries", acts_map_guard.len());
    } else {
      return Err("ACTS_MAP is not a valid object".to_string());
    }

    // --- Step 4: Combine all the data ---
    let combined_response = CombinedLoginResponse {
      return_value: true,
      return_code: 0,
      msg: "success".to_string(),
      character_count: char_count.character_count,
      permission: account_info.permission,
      privilege: account_info.privilege,
      user_no: account_info.user_no,
      user_name: account_info.user_name,
      auth_key: auth_key.auth_key,
      banned: account_info.banned,
      acts_map: None,
      pages_map: None,
      session_cookie: None, 
    };
    
    println!("get_fresh_account_info: Success - returning fresh account info");
    serde_json::to_string(&combined_response)
      .map_err(|e| format!("Failed to serialize response: {}", e))
}

// ─── Launcher self-update ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct LauncherUpdateInfo {
  update_available: bool,
  current_version: String,
  new_version: String,
  installer_url: String,
  autoupdater_url: String,
  /// Linux only: URL to replace launcher-bridge.exe alongside the launcher binary.
  bridge_url: String,
}

/// Compare two dot-separated version strings (e.g. "1.0.1.52" vs "0.0.6").
/// Returns true when `remote` is strictly newer than `local`.
fn is_newer_version(remote: &str, local: &str) -> bool {
  let parse = |v: &str| -> Vec<u32> {
    v.split('.').filter_map(|p| p.parse().ok()).collect()
  };
  let r = parse(remote);
  let l = parse(local);
  let len = r.len().max(l.len());
  for i in 0..len {
    let rv = r.get(i).copied().unwrap_or(0);
    let lv = l.get(i).copied().unwrap_or(0);
    if rv > lv { return true; }
    if rv < lv { return false; }
  }
  false
}

/// Returns the path to `launcher_version.ini` in the same directory as the .exe.
fn get_launcher_version_file_path() -> Result<PathBuf, String> {
  let exe_path = env::current_exe()
    .map_err(|e| format!("Failed to get exe path: {}", e))?;
  let exe_dir = exe_path.parent()
    .ok_or("Failed to get exe directory")?;
  Ok(exe_dir.join("launcher_version.ini"))
}

/// Always writes `compiled_version` to `launcher_version.ini`, overwriting any
/// stale value that may have been left by a different (older/newer) binary.
/// This guarantees the file always reflects what is actually running.
fn read_or_create_launcher_version(compiled_version: &str) -> Result<String, String> {
  let ver_path = get_launcher_version_file_path()?;

  // Overwrite unconditionally — the compiled version IS the truth.
  let mut conf = Ini::new();
  conf.with_section(Some("LAUNCHER"))
    .set("version", compiled_version);
  let mut file = File::create(&ver_path)
    .map_err(|e| format!("Failed to write launcher_version.ini: {}", e))?;
  conf.write_to(&mut file)
    .map_err(|e| format!("Failed to write launcher_version.ini: {}", e))?;
  info!("launcher_version.ini synced to compiled version: {}", compiled_version);

  Ok(compiled_version.to_string())
}

/// Return the local launcher version (reads launcher_version.ini, creates if missing).
#[tauri::command]
fn get_launcher_version(app: tauri::AppHandle) -> Result<String, String> {
  let compiled = app.package_info().version.to_string();
  read_or_create_launcher_version(&compiled)
}

/// Fetch `{LAUNCHER_ACTION_URL}/public/patch/launcher_info.ini`, parse the
/// [LAUNCHER] section and compare with the local `launcher_version.ini`.
/// Creates `launcher_version.ini` from the compiled version if it does not exist.
#[tauri::command]
async fn check_launcher_update(app: tauri::AppHandle) -> Result<LauncherUpdateInfo, String> {
  let base_url = &*LAUNCHER_BASE_URL;
  let info_url = format!("{}/public/patch/launcher_info.ini", base_url);

  let client = reqwest::Client::new();
  let response = client
    .get(&info_url)
    .send()
    .await
    .map_err(|e| format!("Failed to fetch launcher_info.ini: {}", e))?;

  if !response.status().is_success() {
    return Err(format!("Server returned {} for launcher_info.ini", response.status()));
  }

  let text = response
    .text()
    .await
    .map_err(|e| format!("Failed to read launcher_info.ini: {}", e))?;

  let ini = Ini::load_from_str(&text)
    .map_err(|e| format!("Failed to parse launcher_info.ini: {}", e))?;

  let section = ini
    .section(Some("LAUNCHER"))
    .ok_or("Missing [LAUNCHER] section in launcher_info.ini")?;

  // Platform-specific version and installer keys.
  // New format:
  //   win_version / linux_version
  //   win_installer_url / linux_installer_url
  //   linux_bridge_url  (Linux only)
  //   autoupdater_url   (shared)
  //
  // Falls back to the legacy `version` / `installer_url` keys so existing
  // servers that haven't added the new keys keep working.
  #[cfg(target_os = "windows")]
  let (version_key, installer_key) = ("win_version", "win_installer_url");
  #[cfg(not(target_os = "windows"))]
  let (version_key, installer_key) = ("linux_version", "linux_installer_url");

  let server_version = section
    .get(version_key)
    .or_else(|| section.get("version"))
    .ok_or(format!("Missing '{}' (or 'version') key in launcher_info.ini", version_key))?
    .to_string();

  let installer_url = section
    .get(installer_key)
    .or_else(|| section.get("installer_url"))
    .ok_or(format!("Missing '{}' (or 'installer_url') key in launcher_info.ini", installer_key))?
    .to_string();

  let autoupdater_url = section
    .get("autoupdater_url")
    .unwrap_or("")
    .to_string();

  // Linux bridge binary URL — empty string on Windows (unused)
  #[cfg(not(target_os = "windows"))]
  let bridge_url = section.get("linux_bridge_url").unwrap_or("").to_string();
  #[cfg(target_os = "windows")]
  let bridge_url = String::new();

  // Use the compiled package version as fallback when creating the local file
  let compiled_version = app.package_info().version.to_string();
  let local_version = read_or_create_launcher_version(&compiled_version)?;

  let update_available = is_newer_version(&server_version, &local_version);

  Ok(LauncherUpdateInfo {
    update_available,
    current_version: local_version,
    new_version: server_version,
    installer_url,
    autoupdater_url,
    bridge_url,
  })
}

/// Fetch `launcher_info.ini` and download `autoupdater.exe` next to the launcher
/// if it isn't already present. Called silently in the background at startup.
async fn ensure_autoupdater() -> Result<(), String> {
  let exe_path = std::env::current_exe()
    .map_err(|e| format!("Failed to get exe path: {}", e))?;
  let exe_dir = exe_path.parent().ok_or("Failed to get exe directory")?;
  let autoupdater = exe_dir.join("autoupdater.exe");

  if autoupdater.exists() {
    info!("ensure_autoupdater: already present, skipping download.");
    return Ok(());
  }

  let base_url = &*LAUNCHER_BASE_URL;
  let info_url = format!("{}/public/patch/launcher_info.ini", base_url);

  let client = reqwest::Client::new();
  let text = client
    .get(&info_url)
    .send()
    .await
    .map_err(|e| format!("Failed to fetch launcher_info.ini: {}", e))?
    .text()
    .await
    .map_err(|e| format!("Failed to read launcher_info.ini: {}", e))?;

  let ini = Ini::load_from_str(&text)
    .map_err(|e| format!("Failed to parse launcher_info.ini: {}", e))?;

  let autoupdater_url = ini
    .section(Some("LAUNCHER"))
    .and_then(|s| s.get("autoupdater_url"))
    .unwrap_or("")
    .to_string();

  if autoupdater_url.is_empty() {
    return Err("launcher_info.ini has no autoupdater_url — skipping download.".to_string());
  }

  info!("ensure_autoupdater: downloading from {}", autoupdater_url);

  let resp = client
    .get(&autoupdater_url)
    .send()
    .await
    .map_err(|e| format!("Failed to download autoupdater.exe: {}", e))?;

  if !resp.status().is_success() {
    return Err(format!("autoupdater.exe download returned status {}", resp.status()));
  }

  let bytes = resp
    .bytes()
    .await
    .map_err(|e| format!("Failed to read autoupdater.exe bytes: {}", e))?;

  tokio::fs::write(&autoupdater, &bytes)
    .await
    .map_err(|e| format!("Failed to save autoupdater.exe: {}", e))?;

  info!(
    "ensure_autoupdater: saved {} bytes to {}",
    bytes.len(),
    autoupdater.display()
  );
  Ok(())
}

/// Download the new launcher exe, then hand off to autoupdater.exe and exit.
///
/// Emits `"launcher_update_progress"` events while downloading:
/// `{ progress: f64 (0-100), downloaded: u64, total: u64 }`
#[tauri::command]
async fn apply_launcher_update(
  installer_url: String,
  autoupdater_url: String,
  new_version: String,
  #[allow(unused_variables)]
  bridge_url: String,
  app: tauri::AppHandle,
) -> Result<(), String> {
  let client = reqwest::Client::new();

  // ── 1. Resolve path to the launcher exe (this is the file we'll replace) ─
  let exe_path = std::env::current_exe()
    .map_err(|e| format!("Failed to resolve launcher exe path: {}", e))?;
  let exe_dir = exe_path
    .parent()
    .ok_or("Failed to get launcher directory")?;

  // ── 2. Ensure autoupdater.exe is present; download it if needed ──────────
  let autoupdater = exe_dir.join("autoupdater.exe");

  info!("apply_launcher_update: autoupdater path = {}", autoupdater.display());
  info!("apply_launcher_update: autoupdater_url  = '{}'", autoupdater_url);
  info!("apply_launcher_update: installer_url    = '{}'", installer_url);
  info!("apply_launcher_update: new_version      = '{}'", new_version);

  if !autoupdater.exists() {
    if autoupdater_url.is_empty() {
      return Err(format!(
        "autoupdater.exe not found at '{}'. Rebuild the launcher so autoupdater_url is passed, or place autoupdater.exe manually.",
        autoupdater.display()
      ));
    }

    info!("autoupdater.exe not found — downloading from {}", autoupdater_url);
    let au_response = client
      .get(&autoupdater_url)
      .send()
      .await
      .map_err(|e| format!("Failed to download autoupdater.exe: {}", e))?;

    if !au_response.status().is_success() {
      return Err(format!(
        "autoupdater.exe download failed with status: {}",
        au_response.status()
      ));
    }

    let au_bytes = au_response
      .bytes()
      .await
      .map_err(|e| format!("Failed to read autoupdater.exe response: {}", e))?;

    tokio::fs::write(&autoupdater, &au_bytes)
      .await
      .map_err(|e| format!("Failed to save autoupdater.exe: {}", e))?;

    info!("autoupdater.exe downloaded successfully");
  }

  // ── 3. Download the new launcher exe to %TEMP%\launcher_update\ ──────────
  let response = client
    .get(&installer_url)
    .send()
    .await
    .map_err(|e| format!("Failed to start launcher download: {}", e))?;

  if !response.status().is_success() {
    return Err(format!("Launcher download failed with status: {}", response.status()));
  }

  let total_size = response.content_length().unwrap_or(0);

  let temp_dir = std::env::temp_dir().join("launcher_update");
  fs::create_dir_all(&temp_dir)
    .map_err(|e| format!("Failed to create temp directory: {}", e))?;

  let filename = installer_url
    .split('/')
    .last()
    .filter(|s| !s.is_empty())
    .unwrap_or("TeraLauncher_new.exe");

  let new_launcher_path = temp_dir.join(filename);

  let mut dest = tokio::fs::File::create(&new_launcher_path)
    .await
    .map_err(|e| format!("Failed to create temp launcher file: {}", e))?;

  let mut downloaded: u64 = 0;
  let mut stream = response.bytes_stream();

  while let Some(chunk) = stream.next().await {
    let chunk = chunk.map_err(|e| format!("Download stream error: {}", e))?;
    dest
      .write_all(&chunk)
      .await
      .map_err(|e| format!("Failed to write chunk: {}", e))?;
    downloaded += chunk.len() as u64;

    let progress = if total_size > 0 {
      (downloaded as f64 / total_size as f64) * 100.0
    } else {
      0.0
    };

    let _ = app.emit_all(
      "launcher_update_progress",
      json!({ "progress": progress, "downloaded": downloaded, "total": total_size }),
    );
  }

  dest.flush().await.map_err(|e| format!("Failed to flush temp launcher file: {}", e))?;
  drop(dest);

  // ── 4. Get current PID so autoupdater can wait for us to exit ─────────────
  let launcher_pid = std::process::id();

  // ── 4b. On Linux: download the updated launcher-bridge.exe if a URL was given ──
  #[cfg(not(target_os = "windows"))]
  if !bridge_url.is_empty() {
    info!("apply_launcher_update: downloading updated launcher-bridge.exe from {}", bridge_url);
    let bridge_resp = client
      .get(&bridge_url)
      .send()
      .await
      .map_err(|e| format!("Failed to download launcher-bridge.exe: {}", e))?;

    if !bridge_resp.status().is_success() {
      return Err(format!("launcher-bridge.exe download failed with status: {}", bridge_resp.status()));
    }

    let bridge_bytes = bridge_resp
      .bytes()
      .await
      .map_err(|e| format!("Failed to read launcher-bridge.exe bytes: {}", e))?;

    let bridge_path = exe_dir.join("launcher-bridge.exe");
    tokio::fs::write(&bridge_path, &bridge_bytes)
      .await
      .map_err(|e| format!("Failed to save launcher-bridge.exe: {}", e))?;

    info!("apply_launcher_update: launcher-bridge.exe updated ({} bytes)", bridge_bytes.len());
  }

  // ── 5. Spawn autoupdater (hidden window) then exit the launcher ───────────
  //  Args: <new_exe_path> <launcher_pid> <target_exe_path> <new_version>
  let new_launcher_str = new_launcher_path
    .to_str()
    .ok_or("Invalid temp launcher path (non-UTF-8)")?;
  let target_exe_str = exe_path
    .to_str()
    .ok_or("Invalid launcher exe path (non-UTF-8)")?;

  #[cfg(target_os = "windows")]
  {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    std::process::Command::new(&autoupdater)
      .arg(new_launcher_str)
      .arg(launcher_pid.to_string())
      .arg(target_exe_str)
      .arg(&new_version)
      .creation_flags(CREATE_NO_WINDOW)
      .spawn()
      .map_err(|e| format!("Failed to spawn autoupdater: {}", e))?;
  }
  #[cfg(not(target_os = "windows"))]
  {
    std::process::Command::new(&autoupdater)
      .arg(new_launcher_str)
      .arg(launcher_pid.to_string())
      .arg(target_exe_str)
      .arg(&new_version)
      .spawn()
      .map_err(|e| format!("Failed to spawn autoupdater: {}", e))?;
  }

  app.exit(0);
  Ok(())
}

// ── Signup / Captcha commands ─────────────────────────────────────────────────

/// Fetches a new slider captcha from the server.
/// Initializes a plain session client without calling the captcha endpoint.
/// Used when captcha is disabled so that signup still has a valid session client.
#[tauri::command]
async fn init_signup_session() -> Result<(), String> {
  let client = Client::builder()
    .cookie_store(true)
    .build()
    .map_err(|e| e.to_string())?;

  let mut client_guard = SIGNUP_SESSION_CLIENT.lock().await;
  *client_guard = Some(client);
  Ok(())
}

/// Creates a fresh session client (with cookie jar) stored globally so that
/// the subsequent verify_captcha and signup calls share the same session.
#[tauri::command]
async fn get_captcha() -> Result<String, String> {
  let cookie_jar = Arc::new(Jar::default());
  let client = Client::builder()
    .cookie_store(true)
    .cookie_provider(Arc::clone(&cookie_jar))
    .build()
    .map_err(|e| e.to_string())?;

  let base_url = &*LAUNCHER_BASE_URL;
  let captcha_url = format!("{}/launcher/GetCaptcha", base_url);

  let res = client
    .get(&captcha_url)
    .send()
    .await
    .map_err(|e| format!("Failed to get captcha: {}", e))?;

  if !res.status().is_success() {
    return Err(format!("Captcha request failed with status: {}", res.status()));
  }

  let body: serde_json::Value = res
    .json()
    .await
    .map_err(|e| format!("Failed to parse captcha response: {}", e))?;

  let mut client_guard = SIGNUP_SESSION_CLIENT.lock().await;
  *client_guard = Some(client);

  serde_json::to_string(&body)
    .map_err(|e| format!("Failed to serialize captcha: {}", e))
}

/// Posts the slider answer to the captcha endpoint, using the same session
/// created in get_captcha. Returns true if the server accepted the answer.
#[tauri::command]
async fn verify_captcha(answer: i32) -> Result<bool, String> {
  let client_guard = SIGNUP_SESSION_CLIENT.lock().await;
  let client = client_guard
    .as_ref()
    .ok_or_else(|| "No signup session. Call get_captcha first.".to_string())?;

  let base_url = &*LAUNCHER_BASE_URL;
  let captcha_url = format!("{}/launcher/GetCaptcha", base_url);
  let answer_str = answer.to_string();

  let res = client
    .post(&captcha_url)
    .form(&[("answer", answer_str.as_str())])
    .send()
    .await
    .map_err(|e| format!("Failed to verify captcha: {}", e))?;

  if !res.status().is_success() {
    return Err(format!("Captcha verify failed with status: {}", res.status()));
  }

  let verify_res: CaptchaVerifyApiResponse = res
    .json()
    .await
    .map_err(|e| format!("Failed to parse captcha verify response: {}", e))?;

  Ok(verify_res.verified)
}

/// Submits the signup form to the backend. Requires captcha to have been
/// verified first (the same session client must carry captchaVerified = true).
#[tauri::command]
async fn signup(login: String, email: String, password: String) -> Result<String, String> {
  let client_guard = SIGNUP_SESSION_CLIENT.lock().await;
  let client = client_guard
    .as_ref()
    .ok_or_else(|| "No signup session. Complete captcha first.".to_string())?;

  let base_url = &*LAUNCHER_BASE_URL;
  let signup_url = format!("{}/launcher/SignupAction", base_url);

  let res = client
    .post(&signup_url)
    .form(&[
      ("login", login.as_str()),
      ("email", email.as_str()),
      ("password", password.as_str()),
    ])
    .send()
    .await
    .map_err(|e| format!("Failed to submit signup: {}", e))?;

  if !res.status().is_success() {
    return Err(format!("Signup request failed with status: {}", res.status()));
  }

  let signup_res: SignupApiResponse = res
    .json()
    .await
    .map_err(|e| format!("Failed to parse signup response: {}", e))?;

  serde_json::to_string(&signup_res)
    .map_err(|e| format!("Failed to serialize signup response: {}", e))
}

#[tauri::command]
async fn get_portal_config() -> Result<String, String> {
  let client = Client::new();
  let base_url = &*LAUNCHER_BASE_URL;
  let url = format!("{}/launcher/GetPortalConfig", base_url);

  let res = match client.get(&url).send().await {
    Ok(r) => r,
    Err(_) => {
      let default = PortalConfigResponse { registration_disabled: false, captcha_enabled: true, patch_no_check: false };
      return serde_json::to_string(&default).map_err(|e| e.to_string());
    }
  };

  if !res.status().is_success() {
    let default = PortalConfigResponse { registration_disabled: false, captcha_enabled: true, patch_no_check: false };
    return serde_json::to_string(&default).map_err(|e| e.to_string());
  }

  let config: PortalConfigResponse = res
    .json()
    .await
    .map_err(|e| format!("Failed to parse portal config: {}", e))?;

  serde_json::to_string(&config)
    .map_err(|e| format!("Failed to serialize portal config: {}", e))
}

// ─────────────────────────────────────────────────────────────────────────────

fn main() {


  dotenv().ok();

  let (tera_logger, mut tera_log_receiver) = teralib::setup_logging();

  // Configure only the teralib logger
  log::set_boxed_logger(Box::new(tera_logger)).expect("Failed to set logger");
  log::set_max_level(LevelFilter::Info);

  // Create an asynchronous channel for logs
  let (log_sender, mut log_receiver) = mpsc::channel::<String>(100);

  // Create a Tokio runtime
  let rt = Runtime::new().expect("Failed to create Tokio runtime");

  // Spawn a task to receive logs and send them through the channel
  rt.spawn(async move {
    while let Some(log_message) = tera_log_receiver.recv().await {
      println!("Teralib: {}", log_message);
      if let Err(e) = log_sender.send(log_message).await {
        eprintln!("Failed to send log message: {}", e);
      }
    }
  });


  let game_status_receiver = get_game_status_receiver();
  let game_state = GameState {
    status_receiver: Arc::new(Mutex::new(game_status_receiver)),
    is_launching: Arc::new(Mutex::new(false)),
  };

  tauri::Builder
    ::default()
    .manage(game_state)
    .setup(|app| {
      let window = app.get_window("main").unwrap();
      let app_handle = app.handle();
      println!("Tauri setup started");

      #[cfg(debug_assertions)]
      window.open_devtools();

      // Ensure the window is visible and focused regardless of how the process
      // was spawned (e.g. by autoupdater.exe which may pass a minimized show-state).
      let _ = window.unminimize();
      let _ = window.show();
      let _ = window.set_focus();

      // Spawn an asynchronous task to receive logs from the channel and send them to the frontend
      tauri::async_runtime::spawn(async move {
        while let Some(log_message) = log_receiver.recv().await {
          let _ = app_handle.emit_all("log_message", log_message);
        }
      });

      // Sync launcher_version.ini with the compiled version immediately at startup.
      // This overwrites any stale version left by a mismatched binary.
      let compiled_ver = app.package_info().version.to_string();
      if let Err(e) = read_or_create_launcher_version(&compiled_ver) {
        info!("Failed to sync launcher_version.ini at startup: {}", e);
      }

      // Silently ensure autoupdater.exe is present beside the launcher exe.
      // Fetches launcher_info.ini to get autoupdater_url, then downloads if needed.
      tauri::async_runtime::spawn(async {
        if let Err(e) = ensure_autoupdater().await {
          info!("ensure_autoupdater (startup): {}", e);
        }
      });

      println!("Tauri setup completed");


      Ok(())
    })
    .invoke_handler(
      tauri::generate_handler![
        handle_launch_game,
        get_game_status,
        select_game_folder,
        get_game_path_from_config,
        save_game_path_to_config,
        reset_launch_state,
        login,
        set_auth_info,
        get_language_from_config,
        save_language_to_config,
        get_files_to_update,
        get_files_to_update_force,
        update_file,
        handle_logout,
        generate_hash_file,
        check_server_connection,
        check_update_required,
        download_all_files,
        get_client_version,
        check_maintenance_and_notify,
        get_fresh_account_info,
        clear_update_cache,
        check_launcher_update,
        apply_launcher_update,
        get_launcher_version,
        init_signup_session,
        get_captcha,
        verify_captcha,
        signup,
        get_portal_config,
      ]
    )
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}