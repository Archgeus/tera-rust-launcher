# Tera Rust Launcher

![Tera Launcher Interface](https://forum.ragezone.com/attachments/tera-png.264594/)

![Tera maintenance modal](http://ss.archgeus.com/u/pESeD3.png)

![Tera signup form with captcha](https://ss.archgeus.com/u/G7ChKN.png)

![Tera launcher autoupdate feature](https://ss.archgeus.com/u/xvMcjp.png)

## Description
Tera Rust Launcher is a custom game launcher designed for Tera Online. It provides features such as automatic updates, game file verification, and multi-language support.

## Key Features
- User authentication
- Automatic game updates
- File integrity checks with force re-check support
- Launcher self-update via autoupdater.exe (downloads, replaces, restarts)
- Version tracking via `launcher_version.ini`
- Update cache management (clear cached state)
- Multi-language support (English, French, Russian, German)
- Custom game path configuration
- Hash file generation for game files

## Technologies Used
- JavaScript (Tauri framework)
- HTML/CSS
- Anime.js for animations

## Build & Setup

### Prerequisites
- [Rust](https://rustup.rs/) (stable)
- [Node.js](https://nodejs.org/) + npm
- [Tauri CLI](https://tauri.app/v1/guides/getting-started/prerequisites): `npm install -g @tauri-apps/cli@^1.6.0`

---

### 1. Clone the repository

```bash
git clone https://github.com/Archgeus/tera-rust-launcher.git
cd tera-rust-launcher
```

---

### 2. Configure `teralib/src/config/config.json`

This file is the single source of URLs consumed by the launcher at compile time:

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

### 3. Build the autoupdater

```bash
cd autoupdater
cargo build --release
```

Output: `autoupdater/target/release/autoupdater.exe`

Host this file on your server and expose it via `autoupdater_url` in `launcher_info.ini` (see below).

---

### 4. Build the launcher

```bash
cd teralaunch
npm install
npm run tauri build
```

Output: `teralaunch/src-tauri/target/release/teralaunch.exe`  
Installer: `teralaunch/src-tauri/target/release/bundle/`

---

### 5. Deployment folder structure

Place the following files in the same directory as `teralaunch.exe`:

```
TeraLauncher.exe
autoupdater.exe       ← downloaded automatically at startup, or place manually
launcher_version.ini  ← auto-created on first run from the compiled version
config.ini            ← game path and language settings (auto-created on first run)
```

---

### 6. Server-side: `launcher_info.ini`

The launcher fetches this file from:  
`{LAUNCHER_ACTION_URL}/public/patch/launcher_info.ini`

Required format:
```ini
[LAUNCHER]
version=1.0.5
installer_url=http://SERVERIP-URI/public/patch/TeraLauncher.exe
autoupdater_url=http://SERVERIP-URI/public/patch/autoupdater.exe
```

| Key | Description |
|---|---|
| `version` | Latest launcher version. Compared against local `launcher_version.ini`. |
| `installer_url` | Direct download URL for the new launcher `.exe`. |
| `autoupdater_url` | Direct download URL for `autoupdater.exe`. Downloaded automatically if missing. |

When a newer `version` is detected, the launcher downloads the new exe via `installer_url`, then spawns `autoupdater.exe` to replace the running binary and restart.

> [!CAUTION]  
> The launcher is set to auto-update by default. If the launcher opens and closes repeatedly (infinite loop), check your versioning. If the compiled version is **1.0.5** but the configuration is set to **1.0.4**, the auto-updater will trigger continuously.

---

### 7. Version tracking

`launcher_version.ini` is written next to the exe on every launch and after an update:

```ini
[LAUNCHER]
version=1.0.4
```

The compiled version (from `tauri.conf.json` → `package.version`) is always treated as the source of truth.

---

> [!IMPORTANT]  
> In order to make work the launcher SignUp and Captcha, you need to do the following changes into TeraApi

Line 513

Change

```
if (req.session.captchaVerified) {
			next();
		} else {
			next(new ApiError("Captcha error", 15));
		}
```

to

```
if (!captcha || req.session.captchaVerified) {
			next();
		} else {
			next(new ApiError("Captcha error", 15));
		}
```

Add in Line 1041 in src\controllers\portalLauncher.controller.js

```
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

Add in line 164

```
.get("/GetPortalConfig", portalLauncherController.GetPortalConfig(mod))
```

With this changes the Launcher will know when the SignUp and Captcha is enabled/disabled based on the TeraApi configuration.

## Note
This launcher is a custom solution and not officially associated with Tera Online or its publishers.

## Full Tutorial
For a comprehensive guide on how to set up and use this launcher, please refer to the full tutorial available at:

[Tera Rust Launcher Tutorial on Ragezone](https://forum.ragezone.com/threads/teralauncher-100-02-advanced-game-launcher-with-tauri-js.1231496/)

## Credits
Original Author: [TheNak976](https://github.com/TheNak976/tera-rust-launcher)  
TERA Api: [JKQ](https://github.com/justkeepquiet/tera-api)  
Launcher Fork: [Archgeus](https://github.com/Archgeus/tera-rust-launcher)

## Disclaimer
This project is for educational purposes only. Always respect the terms of service of the game and its publishers.
