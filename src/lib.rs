mod curve_library;
mod curves;
mod edit_ops;
mod motion_registry;
mod script_editor;
mod search;
mod storage;
mod track_overlay;
mod ui;
mod undo;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aviutl2::AnyResult;
use parking_lot::{Mutex, RwLock};

use aviutl2::generic::__bridge::GenericSingleton;
use aviutl2::raw_window_handle::HasWindowHandle;
use curve_library::{
    library_file_path, library_file_to_load, merge_libraries, save_library_file, CurveLibrary, PROJECT_SERDE_KEY,
};
use edit_ops::EDIT_HANDLE;
use serde::{Deserialize, Serialize};
use ui::{CurveEditor2App, EditorState, SharedState};

pub const SETTINGS_SERDE_KEY: &str = "curveeditor2/settings";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSettings {
    #[serde(default = "default_slot_id_linked")]
    pub slot_id_linked: bool,
    #[serde(default = "default_track_overlay_enabled")]
    pub track_overlay_enabled: bool,
    /// グリッドの分割数（v0.9.0〜）。古いプロジェクトには無いので既定値で補う。
    #[serde(default = "default_grid_divisions")]
    pub grid_divisions: u32,
}

fn default_slot_id_linked() -> bool {
    true
}

fn default_track_overlay_enabled() -> bool {
    false
}

fn default_grid_divisions() -> u32 {
    ui::GRID_DIVISIONS_DEFAULT
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            slot_id_linked: default_slot_id_linked(),
            track_overlay_enabled: default_track_overlay_enabled(),
            grid_divisions: default_grid_divisions(),
        }
    }
}

/// SDK の既知の WARN（`edit_handle` の「Unknown effect item type」）だけを ERROR 以上に絞る。
fn log_filter(level: aviutl2::tracing::Level) -> aviutl2::tracing_subscriber::filter::Targets {
    aviutl2::tracing_subscriber::filter::Targets::new()
        .with_default(level)
        .with_target("aviutl2::generic::binding::edit_handle", aviutl2::tracing::Level::ERROR)
}

/// AviUtl2 の「ログ」ウィンドウへ tracing を出力する。
fn init_logging() {
    use aviutl2::tracing::Level;
    use aviutl2::tracing_subscriber::prelude::*;

    let level = if cfg!(debug_assertions) { Level::DEBUG } else { Level::INFO };
    // aviutl2-rs 0.40 は本体 2.1.9 の設定項目の種類 17〜19（数値グループ / 設定グループ / セパレーター）を知らず、
    // get_effect_items のたびに「Unknown effect item type」を WARN で出す（その項目は結果から落ちるだけで、
    // 値を持つ項目ではない）。トラック取得のたびに出て、本体のログの大半を占めていた
    // （2026-09-23 の直近 6 本で 1,486 行）。この module だけ ERROR 以上にする（ColorHistory_H と同じ）。
    // 0.46 以降は種別を知っているが、版上げは終了時の panic を直さない（0.47 も Renderer::Glow のまま）ので上げていない
    let filter = log_filter(level);
    let _ = aviutl2::tracing_subscriber::fmt()
        .with_max_level(level)
        .event_format(aviutl2::logger::AviUtl2Formatter)
        .with_writer(aviutl2::logger::AviUtl2LogWriter)
        .finish()
        .with(filter)
        .try_init();
}

#[aviutl2::plugin(GenericPlugin)]
pub struct CurveEditor2Plugin {
    /// 起動時は None。register() で非表示ウィンドウを作成し、表示はメニュー操作時。
    window: Mutex<Option<aviutl2_eframe::EframeWindow>>,
    state: SharedState,
    window_created: AtomicBool,
}

impl CurveEditor2Plugin {
    fn ensure_window(&self) -> AnyResult<()> {
        if self.window_created.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut guard = self.window.lock();
        if guard.is_some() {
            self.window_created.store(true, Ordering::Release);
            return Ok(());
        }
        let state = Arc::clone(&self.state);
        *guard = Some(aviutl2_eframe::EframeWindow::new("CurveEditor2", move |cc, handle| {
            Ok(Box::new(CurveEditor2App::new(cc, handle, state)))
        })?);
        self.window_created.store(true, Ordering::Release);
        Ok(())
    }

    fn open_editor(&self) -> AnyResult<()> {
        self.ensure_window()?;
        let hwnd = {
            let guard = self.window.lock();
            let window = guard
                .as_ref()
                .expect("window set after ensure_window");
            let handle = window.handle()?;
            let raw = handle
                .window_handle()
                .map_err(|e| anyhow::anyhow!("window handle: {e}"))?;
            match raw.as_raw() {
                aviutl2::raw_window_handle::RawWindowHandle::Win32(h) => h.hwnd.get() as isize,
                _ => anyhow::bail!("Win32 以外のウィンドウは未対応"),
            }
        };
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SetForegroundWindow, SW_SHOW};
            let hwnd = HWND(hwnd as _);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(())
    }

    fn open_editor_from_menu() {
        if let Err(e) = Self::with_instance_mut(|plugin| plugin.open_editor()) {
            tracing::error!("CurveEditor2 を開けませんでした: {e:#}");
        }
    }
}

impl aviutl2::generic::GenericPlugin for CurveEditor2Plugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_logging();
        tracing::info!("CurveEditor2 v{} 初期化", env!("CARGO_PKG_VERSION"));
        let state: SharedState = Arc::new(RwLock::new(EditorState::default()));
        Ok(Self {
            window: Mutex::new(None),
            state,
            window_created: AtomicBool::new(false),
        })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "CurveEditor2".to_string(),
            information: format!(
                "CurveEditor2 v{version} - Easing editor / by HexBrowns",
                version = env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        EDIT_HANDLE.init(registry.create_edit_handle());
        // get_effects() は起動直後に呼ぶと enum_effect_name 例外を踏みやすい。
        // 初回トラック取得時に遅延ウォームする（edit_ops 側）。
        match motion_registry::ensure_movements_registered() {
            Ok(n) if n > 0 => {
                tracing::info!("CurveEditor2: aviutl2.ini に Movement を {n} 件登録");
                self.state.write().status_message =
                    motion_registry::RESTART_REQUIRED_MESSAGE.to_string();
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("CurveEditor2: Movement 登録スキップ: {e:#}"),
        }
        // 表示メニュー／ウィンドウ配置パネル用に register_window_client が必要。
        // ウィンドウ本体は非表示のまま起動し、表示はメニュー操作時に行う。
        if let Err(e) = self.ensure_window() {
            tracing::error!("CurveEditor2 ウィンドウ初期化失敗: {e:#}");
        } else {
            let guard = self.window.lock();
            if let Some(window) = guard.as_ref() {
                match window.handle() {
                    Ok(handle) => {
                        if let Err(e) = registry.register_window_client("CurveEditor2", &handle) {
                            tracing::error!("register_window_client 失敗: {e}");
                        }
                    }
                    Err(e) => tracing::error!("CurveEditor2 ハンドル取得失敗: {e:#}"),
                }
            }
        }
        registry.register_edit_menu("CurveEditor2を開く", Self::open_editor_from_menu);
    }

    fn on_project_load(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        let path = project.get_path();
        let settings = project.deserialize::<PluginSettings>(SETTINGS_SERDE_KEY).ok();

        let mut loaded = CurveLibrary::default();
        if let Ok(lib) = project.deserialize::<CurveLibrary>(PROJECT_SERDE_KEY) {
            merge_libraries(&mut loaded, &lib);
        }
        // プロジェクトごとのライブラリ。無ければフォルダに 1 つの以前のものを引き継ぐ（保存はプロジェクトごとの方へ）
        if let Some(lib_path) = library_file_to_load(path.as_deref()) {
            if let Ok(file_lib) = curve_library::load_library_file(&lib_path) {
                merge_libraries(&mut loaded, &file_lib);
            }
        }

        {
            let mut state = self.state.write();
            state.project_path = path.clone();
            if let Some(settings) = settings {
                state.slot_id_linked = settings.slot_id_linked;
                state.track_overlay_enabled = settings.track_overlay_enabled;
                state.grid_divisions = settings
                    .grid_divisions
                    .clamp(ui::GRID_DIVISIONS_MIN, ui::GRID_DIVISIONS_MAX);
            }
            state.load_project_live();
            state.curve_library = loaded;
            state.load_curve_from_library(false);
            if state.slot_id_linked {
                state.sync_slot_to_id();
            }
            // 別プロジェクトのカーブへ Undo で戻れないように、読み込んだ状態を起点にする
            state.reset_undo_history();
            state.status_message = match &path {
                Some(p) => format!(
                    "プロジェクト読込: {} (ライブラリ {} 件) - 表示\u{2192}CurveEditor2",
                    p.display(),
                    state.curve_library.defined_count()
                ),
                None => "プロジェクト読込（パスなし）- 表示\u{2192}CurveEditor2".to_string(),
            };
        }
    }

    fn on_project_save(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        let path = project.get_path();
        let mut state = self.state.write();
        if let Some(p) = path.clone() {
            state.project_path = Some(p);
        }
        let settings = PluginSettings {
            slot_id_linked: state.slot_id_linked,
            track_overlay_enabled: state.track_overlay_enabled,
            grid_divisions: state.grid_divisions,
        };
        if let Err(e) = project.serialize(SETTINGS_SERDE_KEY, &settings) {
            tracing::warn!("settings serialize failed: {e}");
        }
        save_curve_library(project, &state);
        // 外から active に書かれたスロット（ClaudeBridge の set_custom_curve など）を取り込んでから書く。
        // 取り込まないと 1 分ごとの自動バックアップで消える（issues/20260927_external_live_slots_lost_on_save.md）
        let st = &mut *state;
        let project_path = st.project_path.clone();
        match storage::sync_live_to_disk_adopting(
            project_path.as_deref(),
            &mut st.live_slots,
            &mut st.live_disk_base,
        ) {
            Err(e) => st.status_message = format!("保存時Live同期エラー: {e}"),
            Ok(adopted) => {
                if !adopted.is_empty() {
                    tracing::info!(?adopted, "CurveEditor2: 外から書かれた Live スロットを取り込んだ");
                }
                st.status_message = format!(
                    "保存完了 (ライブラリ {} 件)",
                    st.curve_library.defined_count()
                );
            }
        }
    }
}

fn save_curve_library(project: &mut aviutl2::generic::ProjectFile, state: &EditorState) {
    if let Err(e) = project.serialize(PROJECT_SERDE_KEY, &state.curve_library) {
        tracing::warn!("curve library project serialize failed: {e}");
    }
    if let Some(path) = library_file_path(state.project_path.as_deref()) {
        if let Err(e) = save_library_file(&path, &state.curve_library) {
            tracing::warn!("curve library file save failed: {e}");
        }
    }
}

aviutl2::register_generic_plugin!(CurveEditor2Plugin);

#[cfg(test)]
mod log_tests {
    use super::log_filter;
    use aviutl2::tracing::Level;
    use aviutl2::tracing_subscriber::prelude::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// init_logging と同じ組み立てで、SDK の既知の WARN だけが落ち、ほかの WARN は残ること
    #[test]
    fn filter_drops_only_edit_handle_warn() {
        let buf = Buf::default();
        let writer = buf.clone();
        let sub = aviutl2::tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .with_writer(move || writer.clone())
            .finish()
            .with(log_filter(Level::INFO));
        aviutl2::tracing::subscriber::with_default(sub, || {
            aviutl2::tracing::warn!(target: "aviutl2::generic::binding::edit_handle", "Unknown effect item type: 17");
            aviutl2::tracing::error!(target: "aviutl2::generic::binding::edit_handle", "edit handle error");
            aviutl2::tracing::warn!("curve_editor2 warning");
        });
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(!out.contains("Unknown effect item type"), "{out}");
        assert!(out.contains("edit handle error"), "{out}");
        assert!(out.contains("curve_editor2 warning"), "{out}");
    }
}
