# Tera Rust Launcher

![Tera Launcher Interface](https://forum.ragezone.com/attachments/tera-png.264594/)

![Tera maintenance modal](http://ss.archgeus.com/u/pESeD3.png)

![Tera signup form with captcha](https://ss.archgeus.com/u/G7ChKN.png)

![Tera launcher autoupdate feature](https://ss.archgeus.com/u/xvMcjp.png)

## Description
Tera Rust Launcher is a custom game launcher designed for Tera Online. It provides features such as automatic updates, game file verification, and multi-language support. Runs natively on Windows and Linux (Linux runs Tera.exe and launcher-bridge.exe under Wine).

## Key Features
- User authentication with optional captcha and registration toggle (driven by server config)
- Automatic game file updates with hash-based verification and force re-check support
- Launcher self-update via `autoupdater.exe` — downloads new exe, replaces running binary, restarts
- Version tracking via `launcher_version.ini`; compiled version from `tauri.conf.json` is source of truth
- Update cache management (clear cached state between sessions)
- Multi-language support (English, French, Russian, German)
- Custom game path configuration via `config.ini`
- Hash file generation for game files (`hash-file.json`)
- Server list integration via configurable `SERVER_LIST_URL`
- **Linux support**: native Tauri binary (WebKitGTK) + `launcher-bridge.exe` cross-compiled for Wine; Win32 IPC bridged to native launcher via stdin/stdout JSON pipes

## Architecture (multi-crate workspace)

| Crate | Output | Purpose |
|---|---|---|
| `teralib` | library | Core logic: config, auth, file checks, game launch |
| `teralaunch/src-tauri` | `TeraLauncher.exe` / `TeraLauncher` Linux binary | Tauri frontend shell |
| `autoupdater` | `autoupdater.exe` | Waits for launcher to exit, replaces binary, restarts |
| `launcher-bridge` | `launcher-bridge.exe` (Win32) | Wine IPC bridge for Linux; proxies WM_COPYDATA ↔ JSON |

> `launcher-bridge` is excluded from the workspace `[members]` because it must always target `x86_64-pc-windows-gnu`. Build it separately (see below).

## Technologies Used
- Rust (Tauri v1, tokio, prost, winapi)
- JavaScript / HTML / CSS
- Anime.js for animations

---

## Build & Setup

### Prerequisites — Windows

- [Rust](https://rustup.rs/) stable toolchain
- [Node.js](https://nodejs.org/) + npm
- [Tauri CLI v1](https://tauri.app/v1/guides/getting-started/prerequisites): `npm install -g @tauri-apps/cli@^1.6.0`
- [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (usually pre-installed on Windows 10/11)

### Prerequisites — Linux

Run the provided helper script — it detects Debian/Ubuntu (`apt`) or Fedora/RHEL (`dnf`), installs all required packages, creates WebKit pkg-config/`.so` aliases for Ubuntu 24.04+, and adds the Rust cross-compile target:

```bash
chmod +x install-linux-deps.sh
./install-linux-deps.sh
```

What the script installs:
- `libwebkit2gtk-4.0-dev` (or `4.1` + alias shims for Ubuntu 24.04+ / Debian 12+)
- `build-essential`, `pkg-config`, `libssl-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `libsoup2.4-dev`
- `wine`, `wine64`, `winetricks`
- `gcc-mingw-w64-x86-64` (mingw — for cross-compiling `launcher-bridge.exe`)
- Rust target `x86_64-pc-windows-gnu` via `rustup`

Then install Node.js + Tauri CLI:

```bash
# Node via nvm or your distro's package manager
npm install -g @tauri-apps/cli@^1.6.0
```

---

### 1. Clone the repository

```bash
git clone https://github.com/Archgeus/tera-rust-launcher.git
cd tera-rust-launcher
```

---

### 2. Configure `teralib/src/config/config.json`

Baked into the binary at compile time. Edit before building:

```json
{
  "LAUNCHER_ACTION_URL": "http://SERVERIP-URI",
  "HASH_FILE_URL": "http://SERVERIP-URI/public/launcher/hash-file.json",
  "FILE_SERVER_URL": "http://SERVERIP-URI/public",
  "SERVER_LIST_URL": "http://SERVERIP-URI/tera/ServerList.json?lang=en&sort=3",
  "CLIENT_VERSION": "31.04"
}
```

---

### 3. Build — autoupdater

**Windows:**
```bash
cd autoupdater
cargo build --release
# Output: autoupdater/target/release/autoupdater.exe
```

**Linux (cross-compile to Windows exe, runs under Wine):**
```bash
cd autoupdater
cargo build --release --target x86_64-pc-windows-gnu
# Output: autoupdater/target/x86_64-pc-windows-gnu/release/autoupdater.exe
```

Host the output on your server. Expose via `autoupdater_url` in `launcher_info.ini`.

---

### 4. Build — TeraLauncher (Tauri frontend)

**Windows:**
```bash
cd teralaunch
npm install
npm run tauri build
# Output exe:       teralaunch/src-tauri/target/release/TeraLauncher.exe
# Output installer: teralaunch/src-tauri/target/release/bundle/
```

**Linux:**
```bash
cargo build --release -p TeraLauncher
# Output binary: teralaunch/src-tauri/target/release/TeraLauncher
# (native Linux binary using WebKitGTK — no Wine needed for the launcher itself)
```

---

### 5. Build — launcher-bridge (Linux only)

`launcher-bridge` is a Win32 binary that runs under Wine alongside `Tera.exe`. It bridges the game's WM_COPYDATA IPC protocol to the native Linux launcher via stdin/stdout JSON.

**Must be cross-compiled from the workspace root or from inside the `launcher-bridge/` directory:**

```bash
# From workspace root:
cargo build --release --manifest-path launcher-bridge/Cargo.toml --target x86_64-pc-windows-gnu

# OR from inside launcher-bridge/:
cd launcher-bridge
cargo build --release --target x86_64-pc-windows-gnu
# Output: launcher-bridge/target/x86_64-pc-windows-gnu/release/launcher-bridge.exe
```

Copy next to the launcher binary so it can be found at runtime:
```bash
cp launcher-bridge/target/x86_64-pc-windows-gnu/release/launcher-bridge.exe \
   teralaunch/src-tauri/target/release/launcher-bridge.exe
```

---

### 6. Wine prefix setup — Linux

Set up a minimal Wine prefix for Tera.exe and launcher-bridge.exe:

```bash
export WINEPREFIX=~/.tera-wine
export WINEARCH=win64
wineboot --init
```

Point `config.ini` at your Tera installation inside the Wine prefix, e.g.:
```
[CONFIG]
game_path=~/.tera-wine/drive_c/Tera/Client/Binaries/TERA.exe
```

Run the launcher:
```bash
export WINEPREFIX=~/.tera-wine
./teralaunch/src-tauri/target/release/TeraLauncher
```

---

### 8. Deployment folder structure — Windows

Place the following files in the same directory as `TeraLauncher.exe`:

```
TeraLauncher.exe
autoupdater.exe       ← downloaded automatically at startup, or place manually
launcher_version.ini  ← auto-created on first run from compiled version
config.ini            ← game path and language settings (auto-created on first run)
```

---

### 9. Server-side: `launcher_info.ini`

Fetched by the launcher from:
`{LAUNCHER_ACTION_URL}/public/patch/launcher_info.ini`

```ini
[LAUNCHER]
version=1.0.6
win_bin_url=http://SERVERIP-URI/public/patch/launcher_update/windows/TeraLauncher.exe
linux_bin_url=http://SERVERIP-URI/public/patch/launcher_update/linux/TeraLauncher
linux_bridge_url=http://SERVERIP-URI/public/patch/launcher_update/linux/launcher-bridge.exe
autoupdater_url=http://SERVERIP-URI/public/patch/launcher_update/autoupdater.exe
```

| Key | Description |
|---|---|
| `version` | Latest launcher version. Compared against local `launcher_version.ini`. |
| `win_bin_url` | Windows launcher exe download URL. |
| `linux_bin_url` | Linux native launcher binary download URL. |
| `linux_bridge_url` | Linux: `launcher-bridge.exe` (Win32, runs under Wine) download URL. |
| `autoupdater_url` | `autoupdater.exe` download URL. Downloaded automatically if missing. |

When a newer `version` is detected, the launcher downloads the platform-appropriate binary via `win_bin_url` or `linux_bin_url`, then spawns `autoupdater.exe` to replace the running binary and restart. On Linux, `linux_bridge_url` is also refreshed alongside the main binary.

> [!CAUTION]  
> The launcher auto-updates by default. If it opens and closes in a loop, check versioning. If the compiled version is **1.0.6** but `launcher_info.ini` says **1.0.5**, the updater triggers every launch. The compiled version (from `teralaunch/src-tauri/tauri.conf.json` → `package.version`) must be ≤ `launcher_info.ini` version.

---

### 10. Server-side: required `/public/` folder structure

The web server must expose these paths under `FILE_SERVER_URL` (`/public/`):

```
public/
├── patch/
│   ├── launcher_info.ini                        ← launcher version + download URLs
│   └── launcher_update/
│       ├── autoupdater.exe                      ← shared autoupdater (Win32)
│       ├── windows/
│       │   └── TeraLauncher.exe                 ← Windows launcher binary
│       └── linux/
│           ├── TeraLauncher                     ← Linux native launcher binary
│           └── launcher-bridge.exe              ← Win32 IPC bridge (runs under Wine)
└── launcher/
    └── hash-file.json                           ← game file hashes for integrity checks
```

Game patch files referenced by `hash-file.json` are served from `FILE_SERVER_URL` and can be organized however your server is set up; the launcher downloads individual files by the paths recorded in the hash file.

---

### 11. Version tracking

`launcher_version.ini` is written next to the exe on every launch and after an update:

```ini
[LAUNCHER]
version=1.0.6
```

Compiled version from `tauri.conf.json → package.version` is always source of truth.

---

> [!IMPORTANT]  
> To enable SignUp and Captcha support, the TERA Api must expose the `/GetPortalConfig` endpoint. How to do this depends on when you set up your server:

---

#### If you set up your TERA Api **before 21/04/2026** (manual patches applied)

You previously applied manual patches to the TERA Api. Those changes are now superseded by the plugin below. **Revert them first:**

1. In `src/controllers/portalLauncher.controller.js` (around line 513), revert:

```js
if (!captcha || req.session.captchaVerified) {
			next();
		} else {
			next(new ApiError("Captcha error", 15));
		}
```

TO this (original):

```js
if (req.session.captchaVerified) {
			next();
		} else {
			next(new ApiError("Captcha error", 15));
		}
```

2. In `src/controllers/portalLauncher.controller.js`, remove the `GetPortalConfig` export added around line 1041.

```js
module.exports.GetPortalConfig = () => [
	/**
	 * @type {RequestHandler}
	 */
	(req, res) => {
		res.json({
			registrationDisabled: isRegistrationDisabled,
			captchaEnabled: captcha !== null
		});
	}
];
```

4. In the router file `src/routes/portal/launcher.routes.js` (around line 164), remove:
```js
.get("/GetPortalConfig", portalLauncherController.GetPortalConfig(mod))
```

Then install the plugin (see below).

---

#### If you set up your TERA Api **on or after 21/04/2026** (no manual patches needed)

Install the plugin directly — no manual code edits required.

Clone or download the plugin into the `src/plugins/` directory of your TERA Api:

```bash
cd src/plugins
git clone https://github.com/Archgeus/tera-rust-launcher-plugin
```

Or download and extract the ZIP from the GitHub releases page, then place the folder so the structure looks like:

```
src/
└── plugins/
    └── tera-rust-launcher-plugin/
        ├── plugin.js
        └── README.md
```

The plugin is automatically discovered and loaded from `src/plugins/tera-rust-launcher-plugin/` when the application starts — no additional registration required.

The plugin exposes `/GetPortalConfig` and the launcher uses it to know when SignUp and Captcha are enabled/disabled based on the TERA Api configuration.

## WebLink Configuration

Configure external URLs in `src-tauri/WebLink.json`:

```json
{
  "WebUrl": "https://your-website.com",
  "DiscordUrl": "https://discord.gg/your-server",
  "SupportUrl": "https://support.your-domain.com"
}
```

**Fields:**
- **WebUrl** - Main website link
- **DiscordUrl** - Discord server invite
- **SupportUrl** - Support/documentation page

Update URLs before building. Changes apply to launcher links and buttons.

## Note
This launcher is a custom solution and not officially associated with Tera Online or its publishers.

## Full Tutorial (old)
For a comprehensive guide on how to set up and use this launcher, please refer to the full tutorial available at:

[Tera Rust Launcher Tutorial on Ragezone](https://forum.ragezone.com/threads/teralauncher-100-02-advanced-game-launcher-with-tauri-js.1231496/)

## Credits
Original Author: [TheNak976](https://github.com/TheNak976/tera-rust-launcher)  
TERA Api: [JKQ](https://github.com/justkeepquiet/tera-api)  
Launcher Fork: [Archgeus](https://github.com/Archgeus/tera-rust-launcher)

## Disclaimer
This project is for educational purposes only. Always respect the terms of service of the game and its publishers.
