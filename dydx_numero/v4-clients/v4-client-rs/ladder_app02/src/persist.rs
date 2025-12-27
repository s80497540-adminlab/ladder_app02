use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::AppWindow;

/// Bump when you change config schema.
const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,

    // --- core app state ---
    pub current_ticker: String,
    pub mode: String,
    pub time_mode: String,

    // TF selection + candle window
    pub tf_selected: i32,
    pub candle_window_minutes: i32,

    // toggles
    pub show_depth: bool,
    pub show_trades: bool,
    pub show_volume: bool,

    // dom depth
    pub dom_depth_levels: i32,

    // scroll
    pub ui_scroll_x_px: f32,
    pub ui_scroll_y_px: f32,
    pub ui_content_w_px: f32,
    pub ui_content_h_px: f32,

    // chart
    pub chart_x_zoom: f32,
    pub chart_y_zoom: f32,
    pub chart_pan_x: f32,
    pub chart_pan_y: f32,
    pub chart_cursor_x: f32,
    pub chart_cursor_y: f32,
    pub inline_chart_height_px: f32,

    // popout
    pub chart_popout_open: bool,
    pub chart_popout_x_px: f32,
    pub chart_popout_y_px: f32,
    pub chart_popout_w_px: f32,
    pub chart_popout_h_px: f32,

    // settings
    pub settings_network: String,
    pub settings_rpc_endpoint: String,
    pub settings_auto_sign: bool,
    pub settings_session_ttl_minutes: String,

    // panel positions
    pub orderbook_x_px: f32,
    pub orderbook_y_px: f32,
    pub settings_x_px: f32,
    pub settings_y_px: f32,

    // window geometry
    pub window_width_px: f32,
    pub window_height_px: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,

            current_ticker: "ETH-USD".to_string(),
            mode: "Live".to_string(),
            time_mode: "Local".to_string(),

            tf_selected: 4,
            candle_window_minutes: 60,

            show_depth: true,
            show_trades: true,
            show_volume: true,

            dom_depth_levels: 15,

            ui_scroll_x_px: 0.0,
            ui_scroll_y_px: 0.0,
            ui_content_w_px: 1800.0,
            ui_content_h_px: 1050.0,

            chart_x_zoom: 1.0,
            chart_y_zoom: 1.0,
            chart_pan_x: 0.0,
            chart_pan_y: 0.0,
            chart_cursor_x: 0.5,
            chart_cursor_y: 0.5,
            inline_chart_height_px: 160.0,

            chart_popout_open: false,
            chart_popout_x_px: 80.0,
            chart_popout_y_px: 240.0,
            chart_popout_w_px: 760.0,
            chart_popout_h_px: 440.0,

            settings_network: "Testnet".to_string(),
            settings_rpc_endpoint: "".to_string(),
            settings_auto_sign: false,
            settings_session_ttl_minutes: "30".to_string(),

            orderbook_x_px: 16.0,
            orderbook_y_px: 80.0,
            settings_x_px: 392.0,
            settings_y_px: 80.0,

            window_width_px: 1200.0,
            window_height_px: 800.0,
        }
    }
}

pub struct Persistence {
    path: PathBuf,
    last_saved_json: String,
    dirty: std::sync::atomic::AtomicBool,
}

impl Persistence {
    pub fn new() -> Result<Self> {
        let path = default_config_path()?;
        Ok(Self {
            path,
            last_saved_json: String::new(),
            dirty: std::sync::atomic::AtomicBool::new(true),
        })
    }

    pub fn config_path(&self) -> &Path {
        &self.path
    }

    pub fn mark_dirty(&self) {
        self.dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn load(&self) -> AppConfig {
        match read_json::<AppConfig>(&self.path) {
            Ok(mut cfg) => {
                // migrations hook
                if cfg.version == 0 {
                    cfg.version = CONFIG_VERSION;
                }
                cfg
            }
            Err(err) => {
                archive_corrupt(&self.path, &err);
                AppConfig::default()
            }
        }
    }

    pub fn apply_to_ui(cfg: &AppConfig, ui: &AppWindow) {
        ui.set_current_ticker(cfg.current_ticker.clone().into());
        ui.set_mode(cfg.mode.clone().into());
        ui.set_time_mode(cfg.time_mode.clone().into());

        ui.set_tf_selected(cfg.tf_selected);
        ui.set_candle_window_minutes(cfg.candle_window_minutes);

        ui.set_show_depth(cfg.show_depth);
        ui.set_show_trades(cfg.show_trades);
        ui.set_show_volume(cfg.show_volume);

        ui.set_dom_depth_levels(cfg.dom_depth_levels);

        ui.set_ui_scroll_x_px(cfg.ui_scroll_x_px);
        ui.set_ui_scroll_y_px(cfg.ui_scroll_y_px);
        ui.set_ui_content_w_px(cfg.ui_content_w_px);
        ui.set_ui_content_h_px(cfg.ui_content_h_px);

        ui.set_chart_x_zoom(cfg.chart_x_zoom);
        ui.set_chart_y_zoom(cfg.chart_y_zoom);
        ui.set_chart_pan_x(cfg.chart_pan_x);
        ui.set_chart_pan_y(cfg.chart_pan_y);
        ui.set_chart_cursor_x(cfg.chart_cursor_x);
        ui.set_chart_cursor_y(cfg.chart_cursor_y);
        ui.set_inline_chart_height((cfg.inline_chart_height_px).into());

        ui.set_chart_popout_open(cfg.chart_popout_open);
        ui.set_chart_popout_x((cfg.chart_popout_x_px).into());
        ui.set_chart_popout_y((cfg.chart_popout_y_px).into());
        ui.set_chart_popout_w((cfg.chart_popout_w_px).into());
        ui.set_chart_popout_h((cfg.chart_popout_h_px).into());

        ui.set_settings_network(cfg.settings_network.clone().into());
        ui.set_settings_rpc_endpoint(cfg.settings_rpc_endpoint.clone().into());
        ui.set_settings_auto_sign(cfg.settings_auto_sign);
        ui.set_settings_session_ttl_minutes(cfg.settings_session_ttl_minutes.clone().into());

        // panel positions (requires new slint props)
        ui.set_orderbook_x((cfg.orderbook_x_px).into());
        ui.set_orderbook_y((cfg.orderbook_y_px).into());
        ui.set_settings_x((cfg.settings_x_px).into());
        ui.set_settings_y((cfg.settings_y_px).into());

        // window geometry
        ui.set_width((cfg.window_width_px).into());
        ui.set_height((cfg.window_height_px).into());
    }

    pub fn snapshot_from_ui(ui: &AppWindow) -> AppConfig {
        AppConfig {
            version: CONFIG_VERSION,

            current_ticker: ui.get_current_ticker().to_string(),
            mode: ui.get_mode().to_string(),
            time_mode: ui.get_time_mode().to_string(),

            tf_selected: ui.get_tf_selected(),
            candle_window_minutes: ui.get_candle_window_minutes(),

            show_depth: ui.get_show_depth(),
            show_trades: ui.get_show_trades(),
            show_volume: ui.get_show_volume(),

            dom_depth_levels: ui.get_dom_depth_levels(),

            ui_scroll_x_px: ui.get_ui_scroll_x_px(),
            ui_scroll_y_px: ui.get_ui_scroll_y_px(),
            ui_content_w_px: ui.get_ui_content_w_px(),
            ui_content_h_px: ui.get_ui_content_h_px(),

            chart_x_zoom: ui.get_chart_x_zoom(),
            chart_y_zoom: ui.get_chart_y_zoom(),
            chart_pan_x: ui.get_chart_pan_x(),
            chart_pan_y: ui.get_chart_pan_y(),
            chart_cursor_x: ui.get_chart_cursor_x(),
            chart_cursor_y: ui.get_chart_cursor_y(),
            inline_chart_height_px: len_to_px(ui.get_inline_chart_height()),

            chart_popout_open: ui.get_chart_popout_open(),
            chart_popout_x_px: len_to_px(ui.get_chart_popout_x()),
            chart_popout_y_px: len_to_px(ui.get_chart_popout_y()),
            chart_popout_w_px: len_to_px(ui.get_chart_popout_w()),
            chart_popout_h_px: len_to_px(ui.get_chart_popout_h()),

            settings_network: ui.get_settings_network().to_string(),
            settings_rpc_endpoint: ui.get_settings_rpc_endpoint().to_string(),
            settings_auto_sign: ui.get_settings_auto_sign(),
            settings_session_ttl_minutes: ui.get_settings_session_ttl_minutes().to_string(),

            orderbook_x_px: len_to_px(ui.get_orderbook_x()),
            orderbook_y_px: len_to_px(ui.get_orderbook_y()),
            settings_x_px: len_to_px(ui.get_settings_x()),
            settings_y_px: len_to_px(ui.get_settings_y()),

            window_width_px: len_to_px(ui.get_width()),
            window_height_px: len_to_px(ui.get_height()),
        }
    }

    pub fn save_now(&mut self, cfg: &AppConfig) -> Result<()> {
        fs::create_dir_all(self.path.parent().unwrap())
            .with_context(|| format!("create config dir {:?}", self.path.parent()))?;

        let json = serde_json::to_string_pretty(cfg)?;
        if json == self.last_saved_json {
            self.dirty
                .store(false, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }

        // backup previous
        if self.path.exists() {
            let backup = self.path.with_extension("json.bak");
            let _ = fs::copy(&self.path, backup);
        }

        atomic_write(&self.path, json.as_bytes())?;
        self.last_saved_json = json;
        self.dirty
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Autosave loop + close-save + Ctrl+C save
    pub fn start_autosave(mut self, ui_weak: slint::Weak<AppWindow>) -> Result<()> {
        let mut this = self;

        // Save on window close request (best-effort)
        if let Some(ui) = ui_weak.upgrade() {
            let ui_weak2 = ui_weak.clone();
            let path_clone = this.path.clone();
            ui.window().on_close_requested(move || {
                if let Some(ui) = ui_weak2.upgrade() {
                    let cfg = Persistence::snapshot_from_ui(&ui);
                    let _ = fs::create_dir_all(path_clone.parent().unwrap());
                    let _ = atomic_write(
                        &path_clone,
                        serde_json::to_string_pretty(&cfg).unwrap().as_bytes(),
                    );
                }
                slint::CloseRequestResponse::Close
            });
        }

        // Ctrl+C save (nice for dev)
        {
            let ui_weak = ui_weak.clone();
            let path = this.path.clone();
            let _ = ctrlc::set_handler(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    let cfg = Persistence::snapshot_from_ui(&ui);
                    let _ = fs::create_dir_all(path.parent().unwrap());
                    let _ = atomic_write(&path, serde_json::to_string_pretty(&cfg).unwrap().as_bytes());
                }
                std::process::exit(0);
            });
        }

        // Background autosave thread
        std::thread::spawn(move || {
            let tick = Duration::from_millis(500);
            loop {
                std::thread::sleep(tick);

                if !this
                    .dirty
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    continue;
                }

                let Some(ui) = ui_weak.upgrade() else {
                    break;
                };

                let cfg = Persistence::snapshot_from_ui(&ui);
                if let Err(e) = this.save_now(&cfg) {
                    eprintln!("autosave error: {e:?}");
                }
            }
        });

        Ok(())
    }
}

fn default_config_path() -> Result<PathBuf> {
    let proj = ProjectDirs::from("com", "ladder", "ladder_app02")
        .context("ProjectDirs::from returned None")?;
    Ok(proj.config_dir().join("config.json"))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {:?}", path))?;
    let value = serde_json::from_slice::<T>(&bytes).with_context(|| "parse json")?;
    Ok(value)
}

fn archive_corrupt(path: &Path, err: &anyhow::Error) {
    if !path.exists() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let archived = path.with_extension(format!("corrupt.{ts}.json"));
    let _ = fs::rename(path, archived);
    eprintln!("config corrupt; archived. error: {err:?}");
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("no parent dir for config path")?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));

    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create tmp {:?}", tmp))?;
        f.write_all(bytes).with_context(|| "write tmp")?;
        let _ = f.sync_all();
    }

    fs::rename(&tmp, path).with_context(|| format!("rename {:?} -> {:?}", tmp, path))?;
    Ok(())
}

/// Convert Slint length-like values to px f32.
/// This is intentionally generic because Slint’s generated type differs across versions.
fn len_to_px<L: Into<f32>>(l: L) -> f32 {
    l.into()
}
