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
use std::sync::{Arc, OnceLock};

use aviutl2::AnyResult;
use aviutl2_eframe::egui;
use parking_lot::{Mutex, RwLock};

use curve_library::{
    library_file_path, library_file_to_load, merge_libraries, save_library_file, CurveLibrary, PROJECT_SERDE_KEY,
};
use edit_ops::EDIT_HANDLE;
use serde::{Deserialize, Serialize};
use ui::{CurveEditorHApp, EditorState, SharedState};

pub const SETTINGS_SERDE_KEY: &str = "curveeditor2/settings";

/// 本体のイベント（フォーカス・オブジェクト・シーンの変化）で立てる印（v0.9.9）。イベント用スレッドで立て、UI のスレッドで下ろす。
/// 立っているときだけフォーカスの指紋を読む（それまでは見えている間 0.4 秒ごとに読んでいた。`issues/20261010_focus_polled_every_400ms.md`）
pub(crate) static FOCUS_DIRTY: AtomicBool = AtomicBool::new(true);

/// UI のスレッドを起こすための egui の Context（ウィンドウができたら入る）
pub(crate) static EGUI_CTX: OnceLock<egui::Context> = OnceLock::new();

/// 印を立てて UI を起こす。ここから本体を読まない。立っている間は起こし直さない（連続したイベントで描き直しを重ねない）
fn on_host_event() {
    if !FOCUS_DIRTY.swap(true, Ordering::AcqRel) {
        if let Some(ctx) = EGUI_CTX.get() {
            ctx.request_repaint();
        }
    }
}

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
pub struct CurveEditorHPlugin {
    /// 起動時は None。register() で非表示ウィンドウを作成し、表示はメニュー操作時。
    window: Mutex<Option<aviutl2_eframe::EframeWindow>>,
    state: SharedState,
    window_created: AtomicBool,
}

impl CurveEditorHPlugin {
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
        *guard = Some(aviutl2_eframe::EframeWindow::new("CurveEditor_H", move |cc, handle| {
            Ok(Box::new(CurveEditorHApp::new(cc, handle, state)))
        })?);
        self.window_created.store(true, Ordering::Release);
        Ok(())
    }
}

impl aviutl2::generic::GenericPlugin for CurveEditorHPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_logging();
        tracing::info!("CurveEditor_H v{} 初期化", env!("CARGO_PKG_VERSION"));
        // これより新しい .tra2 は、本体に登録されていないとみなす（本体は .tra2 を起動時に読む）
        motion_registry::mark_loaded();
        let state: SharedState = Arc::new(RwLock::new(EditorState::default()));
        Ok(Self {
            window: Mutex::new(None),
            state,
            window_created: AtomicBool::new(false),
        })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "CurveEditor_H".to_string(),
            information: format!(
                "CurveEditor_H v{version} - Easing editor / by HexBrowns",
                version = env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        EDIT_HANDLE.init(registry.create_edit_handle());
        // get_effects() は起動直後に呼ぶと enum_effect_name 例外を踏みやすい。
        // 初回トラック取得時に遅延ウォームする（edit_ops 側）。
        // 移動方法は本体が起動時に .tra2 から登録する。aviutl2.ini には書かない（v0.9.11 までは追記していた）。
        // 登録されていないものがあれば知らせるだけにする（適用の前にも確かめる）
        let missing = motion_registry::missing_movements(&motion_registry::scan_installed());
        if !missing.is_empty() {
            tracing::warn!(
                "CurveEditor_H: 本体に登録されていない移動方法があります（.tra2 を置き直して再起動）: {}",
                missing.join(", ")
            );
            self.state.write().status_message = format!(
                "移動方法 {} 件が本体に登録されていません。Script/CurveEditor_H/@カーブエディタ_H.tra2 を置き直し、AviUtl2 を再起動してください。",
                missing.len()
            );
        }
        // 表示メニュー／ウィンドウ配置パネル用に register_window_client が必要。
        // ウィンドウ本体は非表示のまま起動し、表示は表示メニューの操作時に本体が行う。
        if let Err(e) = self.ensure_window() {
            tracing::error!("CurveEditor_H ウィンドウ初期化失敗: {e:#}");
        } else {
            let guard = self.window.lock();
            if let Some(window) = guard.as_ref() {
                match window.handle() {
                    Ok(handle) => {
                        if let Err(e) = registry.register_window_client("CurveEditor_H", &handle) {
                            tracing::error!("register_window_client 失敗: {e}");
                        }
                    }
                    Err(e) => tracing::error!("CurveEditor_H ハンドル取得失敗: {e:#}"),
                }
            }
        }
        // ウィンドウは表示メニューから開く（メニューから ShowWindow しても開かない。2026-10-10 にユーザーの指摘で外した）
    }

    fn event_change_focus_object(&mut self) {
        on_host_event();
    }

    fn event_update_object_info(&mut self) {
        on_host_event();
    }

    fn event_change_scene_info(&mut self) {
        on_host_event();
    }

    fn on_project_load(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        let path = project.get_path();
        let settings = project.deserialize::<PluginSettings>(SETTINGS_SERDE_KEY).ok();

        // 読めなかったライブラリを、保存で今の中身に上書きしない（ルール `au2-plugin-checklist`「設定ファイルの下位互換」）
        let mut loaded = CurveLibrary::default();
        let mut embedded_keep = None;
        match project.deserialize::<CurveLibrary>(PROJECT_SERDE_KEY) {
            Ok(lib) => merge_libraries(&mut loaded, &lib),
            // キーがあるのに読めない。元の文字列（見出しと分割した中身）を持っておき、保存ではそのまま書き戻す
            Err(e) if project.get_param_string(PROJECT_SERDE_KEY).is_ok() => {
                tracing::warn!("CurveEditor_H: プロジェクトのカーブライブラリを読めませんでした。保存しても書き換えません: {e}");
                embedded_keep = Some(raw_project_params(project, PROJECT_SERDE_KEY));
            }
            // キーが無い（ライブラリを持たないプロジェクト）
            Err(_) => {}
        }
        // プロジェクトごとのライブラリ。無ければフォルダに 1 つの以前のものを引き継ぐ（保存はプロジェクトごとの方へ）
        let mut file_write_blocked = false;
        if let Some(lib_path) = library_file_to_load(path.as_deref()) {
            match curve_library::load_library_file(&lib_path) {
                Ok(file_lib) => merge_libraries(&mut loaded, &file_lib),
                Err(e) => match curve_library::backup_broken_file(&lib_path) {
                    Ok(dest) => tracing::warn!(
                        "CurveEditor_H: カーブライブラリを読めなかったので {} へ退避しました: {e:#}",
                        dest.display()
                    ),
                    Err(be) => {
                        tracing::warn!(
                            "CurveEditor_H: カーブライブラリを読めず、退避もできませんでした。このプロジェクトの間は書きません: {e:#} / {be}"
                        );
                        file_write_blocked = true;
                    }
                },
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
            state.library_embedded_keep = embedded_keep;
            state.library_file_write_blocked = file_write_blocked;
            state.load_curve_from_library(false);
            if state.slot_id_linked {
                state.sync_slot_to_id();
            }
            // 別プロジェクトのカーブへ Undo で戻れないように、読み込んだ状態を起点にする
            state.reset_undo_history();
            state.status_message = match &path {
                Some(p) => format!(
                    "プロジェクト読込: {} (ライブラリ {} 件) - 表示\u{2192}CurveEditor_H",
                    p.display(),
                    state.curve_library.defined_count()
                ),
                None => "プロジェクト読込（パスなし）- 表示\u{2192}CurveEditor_H".to_string(),
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
        let project_path = st.live_write_project_path();
        match storage::sync_live_to_disk_adopting(
            project_path.as_deref(),
            &mut st.live_slots,
            &mut st.live_disk_base,
        ) {
            Err(e) => st.status_message = format!("保存時Live同期エラー: {e}"),
            Ok(adopted) => {
                if !adopted.is_empty() {
                    tracing::info!(?adopted, "CurveEditor_H: 外から書かれた Live スロットを取り込んだ");
                }
                st.status_message = format!(
                    "保存完了 (ライブラリ {} 件)",
                    st.curve_library.defined_count()
                );
            }
        }
    }
}

/// aviutl2-rs の `serialize` が書く文字列（見出しのキーと、`--aviutl2-rs:serde-base64-chunk:{key}:{n}` に分けた中身）を
/// そのまま集める。読めなかったライブラリを保存で消さずに書き戻すため
fn raw_project_params(project: &aviutl2::generic::ProjectFile, key: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(v) = project.get_param_string(key) {
        out.push((key.to_string(), v));
    }
    for i in 0.. {
        let chunk_key = format!("--aviutl2-rs:serde-base64-chunk:{key}:{i}");
        match project.get_param_string(&chunk_key) {
            Ok(v) => out.push((chunk_key, v)),
            Err(_) => break,
        }
    }
    out
}

fn save_curve_library(project: &mut aviutl2::generic::ProjectFile, state: &EditorState) {
    if let Some(raw) = &state.library_embedded_keep {
        // 読めなかった埋め込みのライブラリは、元の文字列のまま書き戻す（新しいカーブはファイルの方に入る）
        for (k, v) in raw {
            if let Err(e) = project.set_param_string(k, v) {
                tracing::warn!("curve library project keep failed: {k}: {e}");
            }
        }
    } else if let Err(e) = project.serialize(PROJECT_SERDE_KEY, &state.curve_library) {
        tracing::warn!("curve library project serialize failed: {e}");
    }
    if state.library_file_write_blocked {
        return;
    }
    if let Some(path) = library_file_path(state.project_path.as_deref()) {
        if let Err(e) = save_library_file(&path, &state.curve_library) {
            tracing::warn!("curve library file save failed: {e}");
        }
    }
}

aviutl2::register_generic_plugin!(CurveEditorHPlugin);

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
            aviutl2::tracing::warn!("curve_editor_h warning");
        });
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(!out.contains("Unknown effect item type"), "{out}");
        assert!(out.contains("edit handle error"), "{out}");
        assert!(out.contains("curve_editor_h warning"), "{out}");
    }
}
