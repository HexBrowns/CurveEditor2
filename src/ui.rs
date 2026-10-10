//! CurveEditor_H egui UI

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use aviutl2::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use aviutl2_eframe::{AviUtl2EframeHandle, eframe, egui};
use parking_lot::RwLock;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

use crate::curve_library::{
    self, AnchorDoc, BezierDoc, CompositeSegmentDoc, CurveDocument, CurveLibrary, SpringDoc,
    MAX_CURVE_ID,
};
use crate::curves::{
    self, Anchor, BezierParams, CompositeCurve, CompositeSegment, CurveMode, SegmentKind,
    SpringParams,
};
use crate::edit_ops::{self, TrackTarget};
use crate::script_editor;
use crate::search::{self, UsageHit};
use crate::storage;
use crate::track_overlay::{self, TrackOverlayData};
use crate::undo::{CurveSnapshot, UndoHistory};

pub type SharedState = Arc<RwLock<EditorState>>;

const Y_MIN_DEFAULT: f64 = -0.2;
const Y_MAX_DEFAULT: f64 = 1.2;
const T_MIN_DEFAULT: f64 = 0.0;
const T_MAX_DEFAULT: f64 = 1.0;
const HANDLE_RADIUS: f32 = 7.0;
/// ハンドルの掴み判定の半径。描画半径より広げて、拡大しなくても掴めるようにする。
const HANDLE_PICK_RADIUS: f32 = HANDLE_RADIUS + 6.0;
const WHEEL_PAN_SENS: f64 = 0.002;
/// グリッドの分割数（縦横共通）。20 で従来の 5% 罫線と同じ。
pub const GRID_DIVISIONS_DEFAULT: u32 = 20;
pub const GRID_DIVISIONS_MIN: u32 = 2;
pub const GRID_DIVISIONS_MAX: u32 = 100;
/// これより線の間隔が狭いときは補助線を描かない（スナップは効く）。
const GRID_MIN_SPACING_PX: f32 = 4.0;

/// グリッドの交点へ丸める。
fn snap_to_grid(value: f64, divisions: u32) -> f64 {
    let n = divisions.max(1) as f64;
    (value * n).round() / n
}

/// 表示用に文字数で切り詰める（UTF-8 バイト境界で panic しない）。
fn truncate_display(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}\u{2026}")
    }
}

/// egui の zoom_delta (>1 = 拡大) を ViewState の factor (<1 = 拡大) に変換。
fn zoom_factor_from_egui(zoom: f32) -> f64 {
    if (zoom - 1.0).abs() < 0.0001 {
        1.0
    } else {
        1.0 / zoom as f64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragTarget {
    BezierStart,
    BezierCp1,
    BezierCp2,
    BezierEnd,
    Anchor(usize),
    CompositeAnchor(usize),
    CompositeCp1(usize),
    CompositeCp2(usize),
}

/// グラフのズーム・パン状態（t・y 両軸）。
#[derive(Debug, Clone, Copy)]
pub struct ViewState {
    pub t_min: f64,
    pub t_max: f64,
    pub y_min: f64,
    pub y_max: f64,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            t_min: T_MIN_DEFAULT,
            t_max: T_MAX_DEFAULT,
            y_min: Y_MIN_DEFAULT,
            y_max: Y_MAX_DEFAULT,
        }
    }
}

impl ViewState {
    fn zoom_y(&mut self, factor: f64, pivot_y: f64) {
        let new_min = pivot_y - (pivot_y - self.y_min) * factor;
        let new_max = pivot_y + (self.y_max - pivot_y) * factor;
        if new_max - new_min > 0.05 && new_max - new_min < 100.0 {
            self.y_min = new_min;
            self.y_max = new_max;
        }
    }

    fn zoom_x(&mut self, factor: f64, pivot_t: f64) {
        let new_min = pivot_t - (pivot_t - self.t_min) * factor;
        let new_max = pivot_t + (self.t_max - pivot_t) * factor;
        if new_max - new_min > 0.01 && new_max - new_min < 100.0 {
            self.t_min = new_min;
            self.t_max = new_max;
        }
    }

    fn pan_y(&mut self, dy_norm: f64) {
        let range = self.y_max - self.y_min;
        self.y_min += dy_norm * range;
        self.y_max += dy_norm * range;
    }

    fn pan_x(&mut self, dx_norm: f64) {
        let range = self.t_max - self.t_min;
        self.t_min += dx_norm * range;
        self.t_max += dx_norm * range;
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// 数値 UI（DragValue / Slider）の操作状況。変更のあったフレームを Live 同期に伝える。
#[derive(Default, Clone, Copy)]
struct NumericEdits {
    changed: bool,
}

impl NumericEdits {
    fn record(&mut self, response: &egui::Response) {
        if response.changed() {
            self.changed = true;
        }
    }

    fn merge(&mut self, other: NumericEdits) {
        self.changed |= other.changed;
    }
}

fn coord_drag(ui: &mut egui::Ui, label: &str, value: &mut f64) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).speed(0.01).fixed_decimals(4))
    })
    .inner
}

enum AnchorPanelMode {
    Multi,
    Composite,
}

/// アンカーポイント専用の数値入力パネル。操作状況を返す。
fn render_anchor_panel(
    ui: &mut egui::Ui,
    s: &mut EditorState,
    mode: AnchorPanelMode,
    layout: LayoutProfile,
) -> NumericEdits {
    ui.label(if layout.compact {
        "アンカーポイント"
    } else {
        "アンカーポイント（数値入力）"
    });

    let count = match mode {
        AnchorPanelMode::Multi => s.multi_anchors.len(),
        AnchorPanelMode::Composite => s.composite.anchors.len(),
    };
    if count == 0 {
        return NumericEdits::default();
    }

    let mut changed = false;
    let mut edits = NumericEdits::default();
    let max_height = if layout.compact { 120.0 } else { 160.0 };

    egui::ScrollArea::vertical()
        .max_height(max_height)
        .show(ui, |ui| {
            for idx in 0..count {
                let is_endpoint = idx == 0 || idx + 1 == count;
                let is_selected = s.selected_anchor == Some(idx);

                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(is_selected, format!("#{}", idx + 1))
                        .clicked()
                    {
                        s.selected_anchor = Some(idx);
                    }

                    ui.label("t");
                    let t_changed = if is_endpoint {
                        let t_val = match mode {
                            AnchorPanelMode::Multi => s.multi_anchors[idx].t,
                            AnchorPanelMode::Composite => s.composite.anchors[idx].t,
                        };
                        ui.label(format!("{t_val:.4}"));
                        false
                    } else {
                        let response = match mode {
                            AnchorPanelMode::Multi => {
                                let anchor = &mut s.multi_anchors[idx];
                                ui.add(
                                    egui::DragValue::new(&mut anchor.t)
                                        .speed(0.01)
                                        .fixed_decimals(4)
                                        .range(0.0..=1.0),
                                )
                            }
                            AnchorPanelMode::Composite => {
                                let anchor = &mut s.composite.anchors[idx];
                                ui.add(
                                    egui::DragValue::new(&mut anchor.t)
                                        .speed(0.01)
                                        .fixed_decimals(4)
                                        .range(0.0..=1.0),
                                )
                            }
                        };
                        edits.record(&response);
                        response.changed()
                    };

                    ui.label("y");
                    let y_changed = {
                        let response = match mode {
                            AnchorPanelMode::Multi => {
                                let anchor = &mut s.multi_anchors[idx];
                                ui.add(
                                    egui::DragValue::new(&mut anchor.y)
                                        .speed(0.01)
                                        .fixed_decimals(4),
                                )
                            }
                            AnchorPanelMode::Composite => {
                                let anchor = &mut s.composite.anchors[idx];
                                ui.add(
                                    egui::DragValue::new(&mut anchor.y)
                                        .speed(0.01)
                                        .fixed_decimals(4),
                                )
                            }
                        };
                        edits.record(&response);
                        response.changed()
                    };

                    if t_changed || y_changed {
                        changed = true;
                        match mode {
                            AnchorPanelMode::Multi => {
                                if !is_endpoint {
                                    let len = s.multi_anchors.len();
                                    if idx > 0 && idx + 1 < len {
                                        let lo = s.multi_anchors[idx - 1].t + 0.02;
                                        let hi = s.multi_anchors[idx + 1].t - 0.02;
                                        s.multi_anchors[idx].t =
                                            s.multi_anchors[idx].t.clamp(lo, hi);
                                    }
                                }
                                curves::sort_anchors(&mut s.multi_anchors);
                            }
                            AnchorPanelMode::Composite => {
                                s.composite.clamp_anchor(idx);
                            }
                        }
                    }
                });
            }
        });

    edits.changed |= changed;
    edits
}

fn handle_plot_wheel(
    view: &mut ViewState,
    plot_rect: egui::Rect,
    hover_pos: Option<egui::Pos2>,
    scroll: egui::Vec2,
    zoom_2d: egui::Vec2,
    mods: egui::Modifiers,
) -> bool {
    let ctrl = mods.ctrl || mods.command;
    let shift = mods.shift;

    // egui は Ctrl+ホイールを smooth_scroll_delta ではなく zoom_delta に変換する。
    if ctrl && shift {
        if (zoom_2d.x - 1.0).abs() > 0.0001 {
            let xn = hover_pos
                .map(|pos| {
                    ((pos.x - plot_rect.left()) / plot_rect.width()).clamp(0.0, 1.0) as f64
                })
                .unwrap_or(0.5);
            let pivot_t = view.t_min + xn * (view.t_max - view.t_min);
            view.zoom_x(zoom_factor_from_egui(zoom_2d.x), pivot_t);
            return true;
        }
        return false;
    }

    if ctrl {
        let z = if (zoom_2d.y - 1.0).abs() >= (zoom_2d.x - 1.0).abs() {
            zoom_2d.y
        } else {
            zoom_2d.x
        };
        if (z - 1.0).abs() > 0.0001 {
            let yn = hover_pos
                .map(|pos| {
                    1.0 - ((pos.y - plot_rect.top()) / plot_rect.height()).clamp(0.0, 1.0) as f64
                })
                .unwrap_or(0.5);
            let pivot_y = view.y_min + yn * (view.y_max - view.y_min);
            view.zoom_y(zoom_factor_from_egui(z), pivot_y);
            return true;
        }
        return false;
    }

    if shift {
        let wheel = if scroll.x.abs() >= scroll.y.abs() {
            scroll.x
        } else {
            scroll.y
        };
        if wheel.abs() < 0.01 {
            return false;
        }
        view.pan_x(wheel as f64 * WHEEL_PAN_SENS);
        return true;
    }

    if scroll.y.abs() < 0.01 {
        return false;
    }
    view.pan_y(scroll.y as f64 * WHEEL_PAN_SENS);
    true
}

#[derive(Debug, Clone)]
pub struct EditorState {
    pub mode: CurveMode,
    pub slot: i32,
    pub live_enabled: bool,
    pub preset_name: String,
    pub script_code: String,
    pub bezier: BezierParams,
    pub spring: SpringParams,
    pub multi_anchors: Vec<Anchor>,
    pub composite: CompositeCurve,
    pub selected_segment: usize,
    pub selected_anchor: Option<usize>,
    pub live_slots: HashMap<i32, String>,
    /// こちらが前回 active に書いた（読んだ）Live。外から書かれたスロットを見分けるのに使う
    /// （`storage::adopt_external_live`）
    pub live_disk_base: HashMap<i32, String>,
    /// プロジェクトの Live が読めなかった。その間はプロジェクトの Live を書かない
    /// （書くと読めなかったファイルを空や別のプロジェクトの中身で上書きする。公開前チェックリストの点検）
    pub live_project_write_blocked: bool,
    /// プロジェクトに埋め込んだライブラリが読めなかったときの元の文字列。保存ではこれをそのまま書き戻す
    pub library_embedded_keep: Option<Vec<(String, String)>>,
    /// ライブラリのファイルが読めず、退避もできなかった。その間はファイルを書かない
    pub library_file_write_blocked: bool,
    pub presets: Vec<(String, String)>,
    pub project_path: Option<PathBuf>,
    pub selected_category: String,
    pub status_message: String,
    pub view: ViewState,
    pub curve_id: u32,
    pub curve_library: CurveLibrary,
    pub search_hits: Vec<UsageHit>,
    pub search_expanded: bool,
    pub search_message: String,
    /// 検索結果の 2 行目（どこから読んだか・クリックの説明）。見出しの行に詰めると切れる（v0.9.5）
    pub search_detail: String,
    pub apply_tracks: Vec<TrackTarget>,
    pub apply_track_index: usize,
    pub slot_id_linked: bool,
    pub track_overlay_enabled: bool,
    pub track_overlay: Option<TrackOverlayData>,
    pub track_overlay_message: String,
    pub overlay_last_refresh_secs: f64,
    pub live_last_sync_secs: f64,
    pub live_dirty: bool,
    pub lua_parse_error: Option<String>,
    /// グリッドの分割数（縦横共通）。Shift ドラッグのスナップ先もこの交点。
    pub grid_divisions: u32,
    /// エディタ内カーブ編集の Undo / Redo（本体の Undo とは別）。
    undo_history: UndoHistory,
    /// `generate_lua` の入力キャッシュ。毎フレーム呼ばれるので、
    /// 入力が変わっていないフレームでは Lua を作り直さない。
    code_cache: Option<CodeCacheKey>,
}

/// `refresh_code_from_mode` の再生成判定に使う入力一式。
/// `script_code` も含めるので、モデル以外の経路で本文が差し替わった場合
/// （モード切替・プリセット読込・ライブラリ読込）も自動で作り直しになる。
#[derive(Debug, Clone, PartialEq)]
struct CodeCacheKey {
    mode: CurveMode,
    script_code: String,
    bezier: BezierParams,
    spring: SpringParams,
    multi_anchors: Vec<Anchor>,
    composite: CompositeCurve,
}

/// Live 同期間隔（秒）。ドラッグ中も追従するが毎フレームのディスク書き込みは避ける。
pub const LIVE_SYNC_INTERVAL_SECS: f64 = 0.15;

/// フォーカスの指紋を読み直す間隔の下限（秒）。本体のイベントが続けて来ても Edit API を連打しない。
pub const FOCUS_POLL_INTERVAL_SECS: f64 = 0.4;

const COMPACT_WIDTH: f32 = 520.0;
const NARROW_WIDTH: f32 = 380.0;

#[derive(Clone, Copy)]
struct LayoutProfile {
    compact: bool,
    narrow: bool,
}

impl LayoutProfile {
    fn from_width(w: f32) -> Self {
        Self {
            compact: w < COMPACT_WIDTH,
            narrow: w < NARROW_WIDTH,
        }
    }
}

impl Default for EditorState {
    fn default() -> Self {
        let mut state = Self {
            mode: CurveMode::Bezier,
            slot: 1,
            live_enabled: true,
            preset_name: String::new(),
            script_code: "return t".to_string(),
            bezier: BezierParams::default(),
            spring: SpringParams::default(),
            multi_anchors: curves::default_multi_anchors(),
            composite: CompositeCurve::default(),
            selected_segment: 0,
            selected_anchor: None,
            live_slots: HashMap::new(),
            // プロジェクトを読み込んだとき（load_project_live）に active の中身で置き換える。
            // ここでファイルを読むとホストの外（単体テスト）でアプリのデータのパスが取れずに落ちる
            live_disk_base: HashMap::new(),
            live_project_write_blocked: false,
            library_embedded_keep: None,
            library_file_write_blocked: false,
            presets: Vec::new(),
            project_path: None,
            selected_category: String::new(),
            status_message: String::new(),
            view: ViewState::default(),
            curve_id: 1,
            curve_library: CurveLibrary::default(),
            search_hits: Vec::new(),
            search_expanded: false,
            search_message: String::new(),
            search_detail: String::new(),
            apply_tracks: Vec::new(),
            apply_track_index: 0,
            slot_id_linked: true,
            track_overlay_enabled: false,
            track_overlay: None,
            track_overlay_message: String::new(),
            overlay_last_refresh_secs: 0.0,
            live_last_sync_secs: 0.0,
            live_dirty: false,
            lua_parse_error: None,
            grid_divisions: GRID_DIVISIONS_DEFAULT,
            undo_history: UndoHistory::default(),
            code_cache: None,
        };
        state.refresh_code_from_mode();
        state.reset_undo_history();
        state
    }
}

impl EditorState {
    pub fn refresh_code_from_mode(&mut self) {
        if self.mode == CurveMode::Script {
            return;
        }
        // 比較だけならアロケーションが起きない。複製は作り直すときだけ。
        let unchanged = self.code_cache.as_ref().is_some_and(|c| {
            c.mode == self.mode
                && c.script_code == self.script_code
                && c.bezier == self.bezier
                && c.spring == self.spring
                && c.multi_anchors == self.multi_anchors
                && c.composite == self.composite
        });
        if unchanged {
            return;
        }
        self.script_code = curves::generate_lua(
            self.mode,
            &self.bezier,
            &self.spring,
            &self.multi_anchors,
            &self.composite,
            &self.script_code,
        );
        self.code_cache = Some(CodeCacheKey {
            mode: self.mode,
            script_code: self.script_code.clone(),
            bezier: self.bezier.clone(),
            spring: self.spring.clone(),
            multi_anchors: self.multi_anchors.clone(),
            composite: self.composite.clone(),
        });
    }

    /// Lua 欄編集中は自動再生成を抑止する（手入力可逆モード共通）。
    pub fn refresh_code_from_mode_unless(&mut self, lua_focused: bool) {
        if lua_focused
            && matches!(
                self.mode,
                CurveMode::Script
                    | CurveMode::Bezier
                    | CurveMode::MultiBezier
                    | CurveMode::Spring
                    | CurveMode::Bounce
                    | CurveMode::Composite
            )
        {
            return;
        }
        self.refresh_code_from_mode();
    }

    /// Lua 欄の内容をモデルへ反映。成功時は `None`、失敗時はエラーメッセージ。
    pub fn try_apply_lua_edits(&mut self) -> Option<String> {
        self.lua_parse_error = None;
        let result = match self.mode {
            CurveMode::Bezier => curves::parse_bezier_from_lua(&self.script_code).map(|p| {
                self.bezier = p;
            }),
            CurveMode::Spring => curves::parse_spring_from_lua(&self.script_code).map(|p| {
                self.spring = p;
            }),
            CurveMode::Bounce => curves::parse_bounce_from_lua(&self.script_code).map(|_| {}),
            CurveMode::MultiBezier => curves::parse_multi_anchors_from_lua(&self.script_code).map(
                |anchors| {
                    self.multi_anchors = anchors;
                    self.selected_anchor = None;
                },
            ),
            CurveMode::Composite => curves::parse_composite_from_lua(&self.script_code).map(
                |composite| {
                    self.composite = composite;
                    curves::normalize_composite(&mut self.composite, &mut self.selected_segment);
                    self.selected_anchor = None;
                },
            ),
            CurveMode::Script => return None,
        };
        match result {
            Ok(()) => {
                self.mark_live_dirty();
                None
            }
            Err(e) => {
                self.lua_parse_error = Some(e.clone());
                Some(e)
            }
        }
    }

    pub fn current_code(&self) -> String {
        curves::generate_lua(
            self.mode,
            &self.bezier,
            &self.spring,
            &self.multi_anchors,
            &self.composite,
            &self.script_code,
        )
    }

    pub fn apply_to_slot(&mut self) -> anyhow::Result<()> {
        if self.slot_id_linked {
            self.sync_slot_to_id();
        }
        let slot = self.effective_live_slot();
        let code = self.current_code();
        self.live_slots.insert(slot, code);
        let project_path = self.live_write_project_path();
        storage::sync_live_to_disk_adopting(
            project_path.as_deref(),
            &mut self.live_slots,
            &mut self.live_disk_base,
        )?;
        self.live_dirty = false;
        self.status_message = format!("スロット {slot} に反映しました");
        Ok(())
    }

    pub fn live_needs_sync(&self) -> bool {
        if !self.live_enabled {
            return false;
        }
        // 起動直後の空スロットでは書かない（Plugin/ 配下 .lua 更新が
        // 「スクリプト・プラグインの追加」ダイアログ嵐を誘発するため）。
        // ユーザー編集で live_dirty が立ってから同期する。
        if !self.live_dirty {
            return false;
        }
        let slot = self.effective_live_slot();
        let code = self.current_code();
        self.live_slots
            .get(&slot)
            .map(|c| c != &code)
            .unwrap_or(true)
    }

    pub fn live_sync_due(&self, now_secs: f64, force: bool) -> bool {
        force || now_secs - self.live_last_sync_secs >= LIVE_SYNC_INTERVAL_SECS
    }

    pub fn mark_live_dirty(&mut self) {
        self.live_dirty = true;
    }

    pub fn effective_live_slot(&self) -> i32 {
        if self.slot_id_linked {
            self.curve_id as i32
        } else {
            self.slot
        }
    }

    pub fn sync_slot_to_id(&mut self) {
        self.slot = self.curve_id as i32;
    }

    pub fn sync_id_to_slot(&mut self) {
        if self.slot >= 1 {
            self.curve_id = (self.slot as u32).min(MAX_CURVE_ID);
        }
    }

    pub fn set_curve_id(&mut self, id: u32) {
        self.curve_id = id.clamp(1, MAX_CURVE_ID);
        if self.slot_id_linked {
            self.sync_slot_to_id();
        }
    }

    pub fn set_slot(&mut self, slot: i32) {
        self.slot = slot.max(1);
        if self.slot_id_linked {
            self.sync_id_to_slot();
        }
    }

    pub fn reload_presets(&mut self) {
        match storage::load_presets() {
            Ok(p) => {
                self.presets = p;
                self.status_message = "プリセットを読み込みました".to_string();
            }
            Err(e) => self.status_message = format!("プリセット読込エラー: {e}"),
        }
    }

    pub fn load_project_live(&mut self) {
        match storage::load_live_for_project(self.project_path.as_deref()) {
            Ok(slots) => {
                self.live_slots = slots;
                let slot = self.effective_live_slot();
                if let Some(code) = self.live_slots.get(&slot) {
                    self.script_code = code.clone();
                }
                // tra2 が読むのは active だけなので、読み込んだプロジェクトのカーブをすぐ書き出す。
                // 書かないと、次に反映・保存するまで別のプロジェクトで最後に書いたカーブで描画・出力される
                // （issues/20260927_active_live_differs_from_saved_project.md）
                if let Err(e) = storage::write_active_live(&self.live_slots) {
                    tracing::warn!("CurveEditor_H: active Live 書き出し失敗: {e:#}");
                    self.status_message = format!("Live書出エラー: {e}");
                }
                // 以後、この中身から変わったスロットを「外から書かれたもの」とみなす
                self.live_disk_base = storage::read_active_live();
            }
            Err(e) => {
                // 前のプロジェクトのスロットを残すと、保存でそれがこのプロジェクトの Live に書かれる。
                // 空にして、このプロジェクトの Live は書かない（読めなかったファイルを残す）
                tracing::warn!("CurveEditor_H: プロジェクトの Live を読めませんでした。このプロジェクトの Live は書きません: {e:#}");
                self.live_slots.clear();
                self.live_project_write_blocked = true;
                if let Err(e) = storage::write_active_live(&self.live_slots) {
                    tracing::warn!("CurveEditor_H: active Live 書き出し失敗: {e:#}");
                }
                self.live_disk_base = storage::read_active_live();
                self.status_message = format!("Live読込エラー（このプロジェクトの Live は書きません）: {e}");
                return;
            }
        }
        self.live_project_write_blocked = false;
    }

    /// Live を書くときのプロジェクトのパス。読めなかったプロジェクトなら None（active だけ書く）
    pub fn live_write_project_path(&self) -> Option<PathBuf> {
        if self.live_project_write_blocked {
            None
        } else {
            self.project_path.clone()
        }
    }

    pub fn to_document(&self) -> CurveDocument {
        CurveDocument {
            mode: mode_to_str(self.mode).to_string(),
            script_code: self.script_code.clone(),
            bezier: BezierDoc {
                y0: self.bezier.y0,
                x1: self.bezier.x1,
                y1: self.bezier.y1,
                x2: self.bezier.x2,
                y2: self.bezier.y2,
                y3: self.bezier.y3,
            },
            spring: SpringDoc {
                zeta: self.spring.zeta,
                omega: self.spring.omega,
            },
            multi_anchors: self
                .multi_anchors
                .iter()
                .map(|a| AnchorDoc { t: a.t, y: a.y })
                .collect(),
            composite_anchors: self
                .composite
                .anchors
                .iter()
                .map(|a| AnchorDoc { t: a.t, y: a.y })
                .collect(),
            composite_segments: self
                .composite
                .segments
                .iter()
                .map(|s| CompositeSegmentDoc {
                    kind: segment_kind_to_str(s.kind).to_string(),
                    cx1: s.cx1,
                    cy1: s.cy1,
                    cx2: s.cx2,
                    cy2: s.cy2,
                    zeta: s.zeta,
                    omega: s.omega,
                })
                .collect(),
            selected_segment: self.selected_segment,
        }
    }

    pub fn apply_document(&mut self, doc: &CurveDocument) {
        self.mode = str_to_mode(&doc.mode);
        self.script_code = doc.script_code.clone();
        self.bezier = BezierParams {
            y0: doc.bezier.y0,
            x1: doc.bezier.x1,
            y1: doc.bezier.y1,
            x2: doc.bezier.x2,
            y2: doc.bezier.y2,
            y3: doc.bezier.y3,
        };
        self.spring = SpringParams {
            zeta: doc.spring.zeta,
            omega: doc.spring.omega,
        };
        self.multi_anchors = doc
            .multi_anchors
            .iter()
            .map(|a| Anchor::new(a.t, a.y))
            .collect();
        self.composite.anchors = doc
            .composite_anchors
            .iter()
            .map(|a| Anchor::new(a.t, a.y))
            .collect();
        self.composite.segments = doc
            .composite_segments
            .iter()
            .map(|s| CompositeSegment {
                kind: str_to_segment_kind(&s.kind),
                cx1: s.cx1,
                cy1: s.cy1,
                cx2: s.cx2,
                cy2: s.cy2,
                zeta: s.zeta,
                omega: s.omega,
            })
            .collect();
        self.selected_segment = doc.selected_segment;
        if self.composite.anchors.len() < 2 {
            self.composite = CompositeCurve::default();
        }
        if self.composite.segments.is_empty() {
            self.composite.segments.push(CompositeSegment::default());
        }
        self.selected_segment = self
            .selected_segment
            .min(self.composite.segments.len().saturating_sub(1));
        self.refresh_code_from_mode();
    }

    pub fn load_curve_from_library(&mut self, sync_live: bool) {
        if self.slot_id_linked {
            self.sync_slot_to_id();
        }
        if let Some(doc) = self.curve_library.get(self.curve_id).cloned() {
            self.apply_document(&doc);
            self.status_message = format!("カーブ ID {} を読み込みました", self.curve_id);
            if sync_live && self.slot_id_linked {
                if let Err(e) = self.apply_to_slot() {
                    self.status_message = format!(
                        "カーブ ID {} を読み込みました（Live同期エラー: {e}）",
                        self.curve_id
                    );
                }
            } else if sync_live && self.live_enabled {
                self.mark_live_dirty();
            }
        } else {
            self.status_message = format!("カーブ ID {} はライブラリに未定義です", self.curve_id);
        }
    }

    pub fn save_curve_to_library(&mut self) {
        let id = self.curve_id;
        if !(1..=MAX_CURVE_ID).contains(&id) {
            self.status_message = format!("カーブ ID は 1〜{MAX_CURVE_ID} の範囲で指定してください");
            return;
        }
        if self.slot_id_linked {
            self.sync_slot_to_id();
        }
        let doc = self.to_document();
        self.curve_library.insert(id, doc);
        if let Some(path) = curve_library::library_file_path(self.project_path.as_deref()) {
            if let Err(e) = curve_library::save_library_file(&path, &self.curve_library) {
                self.status_message = format!("ライブラリ保存エラー: {e}");
                return;
            }
        }
        let mut msg = format!(
            "カーブ ID {id} をライブラリに保存しました (計 {} 件)",
            self.curve_library.defined_count()
        );
        if self.slot_id_linked {
            match self.apply_to_slot() {
                Ok(()) => msg.push_str(&format!(" → Liveスロット {id} に同期")),
                Err(e) => msg.push_str(&format!("（Live同期エラー: {e}）")),
            }
        }
        self.status_message = msg;
    }

    pub fn delete_curve_from_library(&mut self) {
        if self.curve_library.remove(self.curve_id) {
            if let Some(path) = curve_library::library_file_path(self.project_path.as_deref()) {
                let _ = curve_library::save_library_file(&path, &self.curve_library);
            }
            self.status_message = format!(
                "カーブ ID {} をライブラリから削除しました",
                self.curve_id
            );
        } else {
            self.status_message = format!("カーブ ID {} はライブラリに存在しません", self.curve_id);
        }
    }

    /// 使用箇所検索のクエリと、ほかのシーンを読む保存済みファイル。
    /// 本体を読む部分（`edit_ops::search_current_scene`）は状態のロックの外で呼ぶ（`CurveEditorHApp::usage_search_now`）。
    pub fn usage_search_plan(&self) -> (search::SearchQuery, Option<std::path::PathBuf>) {
        let query = search::build_search_query(
            &self.preset_name,
            self.mode,
            self.effective_live_slot(),
        );
        (query, self.project_path.clone())
    }

    pub fn set_usage_search_result(&mut self, hits: Vec<UsageHit>, message: String, detail: String) {
        self.search_hits = hits;
        self.search_expanded = true;
        self.search_message = message;
        self.search_detail = detail;
    }

    fn curve_snapshot(&self) -> CurveSnapshot {
        CurveSnapshot {
            mode: self.mode,
            script_code: self.script_code.clone(),
            bezier: self.bezier.clone(),
            spring: self.spring.clone(),
            multi_anchors: self.multi_anchors.clone(),
            composite: self.composite.clone(),
        }
    }

    /// 複製せずに比べる（毎フレーム呼ばれる）。
    fn curve_equals(&self, snap: &CurveSnapshot) -> bool {
        self.mode == snap.mode
            && self.script_code == snap.script_code
            && self.bezier == snap.bezier
            && self.spring == snap.spring
            && self.multi_anchors == snap.multi_anchors
            && self.composite == snap.composite
    }

    fn restore_curve_snapshot(&mut self, snap: CurveSnapshot) {
        self.mode = snap.mode;
        self.script_code = snap.script_code;
        self.bezier = snap.bezier;
        self.spring = snap.spring;
        self.multi_anchors = snap.multi_anchors;
        self.composite = snap.composite;
        curves::normalize_composite(&mut self.composite, &mut self.selected_segment);
        self.selected_anchor = None;
        self.lua_parse_error = None;
        self.refresh_code_from_mode();
        // Live ON のときはエディタだけでなく Live スロットも戻す
        self.mark_live_dirty();
    }

    /// 履歴を捨てて、現在のカーブを起点にする（プロジェクト読込時）。
    pub fn reset_undo_history(&mut self) {
        let current = self.curve_snapshot();
        self.undo_history.reset(current);
    }

    /// 前回確定した状態から変わっていれば 1 段として確定する。
    /// 操作の途中（ドラッグ中・テキスト入力中）には呼ばないこと。1 回の操作が 1 段になる。
    pub fn commit_undo_step(&mut self) -> bool {
        let unchanged = self
            .undo_history
            .baseline()
            .is_some_and(|base| self.curve_equals(base));
        if unchanged {
            return false;
        }
        let current = self.curve_snapshot();
        self.undo_history.commit(current)
    }

    pub fn can_undo(&self) -> bool {
        self.undo_history.has_undo()
            || self
                .undo_history
                .baseline()
                .is_some_and(|base| !self.curve_equals(base))
    }

    pub fn can_redo(&self) -> bool {
        self.undo_history.has_redo()
    }

    pub fn undo_edit(&mut self) -> bool {
        self.commit_undo_step();
        match self.undo_history.undo() {
            Some(previous) => {
                self.restore_curve_snapshot(previous);
                self.status_message = "元に戻しました（カーブ編集）".to_string();
                true
            }
            None => false,
        }
    }

    pub fn redo_edit(&mut self) -> bool {
        // 未確定の編集があれば、それが最新。Redo の対象は無くなる。
        if self.commit_undo_step() {
            return false;
        }
        match self.undo_history.redo() {
            Some(next) => {
                self.restore_curve_snapshot(next);
                self.status_message = "やり直しました（カーブ編集）".to_string();
                true
            }
            None => false,
        }
    }

    pub fn apply_tracks_fetched(&mut self, tracks: Vec<TrackTarget>) {
        self.apply_tracks = tracks;
        self.apply_track_index = 0;
        self.status_message = format!(
            "トラック {} 件を取得しました",
            self.apply_tracks.len()
        );
        if self.track_overlay_enabled {
            self.refresh_overlay_from_cache(0.0);
        }
    }

    pub fn refresh_overlay_from_cache(&mut self, now_secs: f64) {
        if !self.track_overlay_enabled {
            self.track_overlay = None;
            self.track_overlay_message.clear();
            return;
        }
        let Some(target) = self.apply_tracks.get(self.apply_track_index).cloned() else {
            self.track_overlay = None;
            self.track_overlay_message =
                "トラックオーバーレイ: 「トラック取得」で対象を選んでください".into();
            return;
        };
        let result = track_overlay::build_overlay_from_cache(&target);
        self.apply_overlay_fetch(now_secs, result);
    }

    pub fn apply_overlay_fetch(
        &mut self,
        now_secs: f64,
        result: Result<TrackOverlayData, String>,
    ) {
        self.overlay_last_refresh_secs = now_secs;
        if !self.track_overlay_enabled {
            self.track_overlay = None;
            self.track_overlay_message.clear();
            return;
        }
        match result {
            Ok(data) => {
                self.track_overlay_message = format!(
                    "F{}-{} / {} → {:.3}〜{:.3}",
                    data.frame_start,
                    data.frame_end,
                    data.track_label,
                    data.start_value,
                    data.end_value
                );
                self.track_overlay = Some(data);
            }
            Err(e) => {
                // 取得失敗時も直前のオーバーレイを残す（表示が突然消えないようにする）
                self.track_overlay_message = format!("トラックオーバーレイ: {e}");
            }
        }
    }

    pub fn clear_overlay_target_hint(&mut self) {
        if self.track_overlay_enabled && self.apply_tracks.is_empty() {
            self.track_overlay = None;
            self.track_overlay_message =
                "トラックオーバーレイ: 「トラック取得」で対象を選んでください".into();
        }
    }

    pub fn focus_search_hit(&mut self, index: usize) -> Option<UsageHit> {
        self.search_hits.get(index).cloned()
    }
}

fn mode_to_str(mode: CurveMode) -> &'static str {
    match mode {
        CurveMode::Bezier => "bezier",
        CurveMode::MultiBezier => "multi_bezier",
        CurveMode::Spring => "spring",
        CurveMode::Bounce => "bounce",
        CurveMode::Composite => "composite",
        CurveMode::Script => "script",
    }
}

fn str_to_mode(s: &str) -> CurveMode {
    match s {
        "multi_bezier" => CurveMode::MultiBezier,
        "spring" => CurveMode::Spring,
        "bounce" => CurveMode::Bounce,
        "composite" => CurveMode::Composite,
        "script" => CurveMode::Script,
        _ => CurveMode::Bezier,
    }
}

fn segment_kind_to_str(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Linear => "linear",
        SegmentKind::Bezier => "bezier",
        SegmentKind::Spring => "spring",
        SegmentKind::Bounce => "bounce",
    }
}

fn str_to_segment_kind(s: &str) -> SegmentKind {
    match s {
        "linear" => SegmentKind::Linear,
        "spring" => SegmentKind::Spring,
        "bounce" => SegmentKind::Bounce,
        _ => SegmentKind::Bezier,
    }
}

pub struct CurveEditorHApp {
    handle: AviUtl2EframeHandle,
    state: SharedState,
    version: String,
    dragging: Option<DragTarget>,
    presets_panel_open: bool,
    /// Lua 欄（下パネル）を開いているか。閉じるとグラフがその高さを受け取る。
    lua_panel_open: bool,
    /// グラフ集中モード: 操作パネルと Lua 欄を畳んでグラフだけにする。
    graph_focus_mode: bool,
    lua_editor_focused: bool,
    pending_overlay_refresh: bool,
    pending_fetched_tracks: Option<Vec<TrackTarget>>,
    pending_fetch_error: Option<String>,
    /// 自動トラック更新用: 前回観測したフォーカス指紋
    last_focus_fingerprint: Option<String>,
    last_focus_poll_secs: f64,
}

impl CurveEditorHApp {
    fn run_deferred_api_work(&mut self, ctx: &egui::Context) {
        if let Some(tracks) = self.pending_fetched_tracks.take() {
            self.state.write().apply_tracks_fetched(tracks);
        }
        if let Some(err) = self.pending_fetch_error.take() {
            self.state.write().status_message = err;
        }

        if self.pending_overlay_refresh {
            self.pending_overlay_refresh = false;
            let now = ctx.input(|i| i.time);
            self.state.write().refresh_overlay_from_cache(now);
        }
    }

    /// quantizer.aux2 と同様、ボタン押下時に egui スレッドから同期的に Edit API を呼ぶ。
    fn fetch_tracks_now(&mut self, ctx: &egui::Context) {
        self.state.write().status_message = "トラック取得中\u{2026}".into();
        let started = std::time::Instant::now();
        match edit_ops::catch_edit_panic("list_focused_object_tracks", edit_ops::list_focused_object_tracks) {
            Ok(tracks) => {
                self.pending_fetched_tracks = Some(tracks);
            }
            Err(e) => self.pending_fetch_error = Some(e),
        }
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            "fetch tracks API done (UI update deferred)"
        );
        ctx.request_repaint();
    }

    fn apply_track_now(&mut self) {
        let plan = {
            let s = self.state.read();
            match s.apply_tracks.get(s.apply_track_index).cloned() {
                Some(target) => {
                    let (section, slot) = edit_ops::motion_section_for(
                        s.mode,
                        &s.preset_name,
                        s.effective_live_slot(),
                    );
                    let live_sync =
                        (section == search::LIVE_SECTION && slot > 0).then(|| {
                            (
                                s.current_code(),
                                s.effective_live_slot(),
                                s.live_write_project_path(),
                            )
                        });
                    Some((target, section, slot, live_sync))
                }
                None => None,
            }
        };

        let Some((target, section, slot, live_sync)) = plan else {
            self.state.write().status_message =
                "適用先トラックがありません。「トラック取得」を押してから「このトラックに適用」してください。"
                    .into();
            return;
        };

        if let Some((code, live_slot, project_path)) = live_sync {
            if code.trim().is_empty() {
                self.state.write().status_message =
                    "Live スロット用のコードが空です。カーブを編集してから「このトラックに適用」してください。"
                        .into();
                return;
            }
            let (mut slots, mut base) = {
                let s = self.state.read();
                (s.live_slots.clone(), s.live_disk_base.clone())
            };
            slots.insert(live_slot, code);
            if let Err(e) =
                storage::sync_live_to_disk_adopting(project_path.as_deref(), &mut slots, &mut base)
            {
                self.state.write().status_message = format!("Live同期エラー: {e}");
                return;
            }
            {
                let mut s = self.state.write();
                s.live_slots = slots;
                s.live_disk_base = base;
            }
        }

        let target_label = target.label.clone();
        let section_for_log = section.clone();
        // 移動方法が付いていなかったトラックは開始＝終了で書き込まれる。
        // そのままだとカーブが効かないので、成功メッセージで次の一手を案内する。
        let was_fixed_value = edit_ops::track_has_no_motion(&target.track_value);
        tracing::info!(track = %target_label, section = %section_for_log, "apply_to_track start");
        let started = std::time::Instant::now();
        let index = self.state.read().apply_track_index;
        let status = match edit_ops::catch_edit_panic("apply_to_track", || {
            edit_ops::apply_to_track(&target, &section, slot)
        }) {
            Ok(written) => {
                // 取得済みキャッシュも新しい値に合わせる。
                // 古い値のまま次を適用すると、開始・終了値を古い方から作り直してしまう。
                let mut s = self.state.write();
                if let Some(slot_target) = s.apply_tracks.get_mut(index) {
                    if slot_target.label == target_label {
                        slot_target.track_value = written;
                    }
                }
                if was_fixed_value {
                    format!(
                        "「{target_label}」に適用しました ({section_for_log})。開始値＝終了値のままなので、オブジェクト設定で終了値を変えてください"
                    )
                } else {
                    format!("「{target_label}」に適用しました ({section_for_log})")
                }
            }
            Err(e) => e,
        };
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            "apply_to_track done (sync egui)"
        );
        self.state.write().status_message = status;
    }

    /// 使用箇所検索（v0.9.5）。今のシーンは本体の今の状態から、ほかのシーンは保存済みのファイルから読む。
    /// `call_read_section` を状態のロックを持ったまま呼ばない（`apply_track_now` と同じ）。
    fn usage_search_now(&mut self) {
        let (query, path) = self.state.read().usage_search_plan();
        let live = edit_ops::catch_edit_panic("search_current_scene", || {
            edit_ops::search_current_scene(&query)
        });
        let saved = path
            .as_ref()
            .map(|p| search::search_aup2_file(p, &query))
            .unwrap_or_default();
        let (hits, source) = match live {
            Ok((scene, live_hits)) => (
                search::merge_live_and_saved(scene, live_hits, saved),
                if path.is_some() {
                    "今のシーンは編集中の状態、ほかのシーンは保存済みのファイルを読みました"
                } else {
                    "今のシーンだけを読みました（プロジェクトが未保存）"
                },
            ),
            Err(e) => {
                tracing::warn!(error = %e, "usage search: read section failed, falling back to saved file");
                (saved, "本体から読めなかったので、保存済みのファイルだけを読みました")
            }
        };
        let message = format!("{} 件（セクション: {}）", hits.len(), query.section);
        let detail = if hits.is_empty() {
            source.to_string()
        } else {
            format!("{source}。行をクリックするとそのオブジェクトを選びます")
        };
        self.state.write().set_usage_search_result(hits, message, detail);
    }

    fn focus_usage_hit_now(&mut self, hit: UsageHit) {
        let line = hit.format_line();
        let status = match edit_ops::catch_edit_panic("focus_usage_hit", || {
            edit_ops::focus_usage_hit(&hit)
        }) {
            Ok(()) => format!("オブジェクトをフォーカスしました: {line}"),
            Err(e) => e,
        };
        self.state.write().status_message = status;
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        handle: AviUtl2EframeHandle,
        state: SharedState,
    ) -> Self {
        // 本体のイベントから UI を起こせるようにする（`lib.rs` の `on_host_event`）
        let _ = crate::EGUI_CTX.set(cc.egui_ctx.clone());
        cc.egui_ctx.all_styles_mut(|style| {
            style.visuals = aviutl2_eframe::aviutl2_visuals();
        });
        // Monospace（Lua 欄）に日本語グリフが無いと □ になるため、Control フォントをフォールバックに追加する。
        let mut fonts = aviutl2_eframe::aviutl2_fonts();
        if fonts.font_data.contains_key("aviutl2::Control") {
            let mono = fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default();
            if !mono.iter().any(|n| n == "aviutl2::Control") {
                mono.push("aviutl2::Control".to_string());
            }
        }
        cc.egui_ctx.set_fonts(fonts);

        {
            let mut s = state.write();
            s.reload_presets();
        }

        Self {
            handle,
            state,
            version: env!("CARGO_PKG_VERSION").to_string(),
            dragging: None,
            presets_panel_open: true,
            lua_panel_open: true,
            graph_focus_mode: false,
            lua_editor_focused: false,
            pending_overlay_refresh: false,
            pending_fetched_tracks: None,
            pending_fetch_error: None,
            last_focus_fingerprint: None,
            last_focus_poll_secs: 0.0,
        }
    }

    /// ウィンドウが表示中か（非表示時は Edit API ポーリングしない）。
    fn is_editor_window_visible(&self) -> bool {
        let Ok(handle) = self.handle.window_handle() else {
            return false;
        };
        match handle.as_raw() {
            RawWindowHandle::Win32(h) => unsafe {
                IsWindowVisible(HWND(h.hwnd.get() as _)).as_bool()
            },
            _ => false,
        }
    }

    /// フォーカスオブジェクトが変わったらトラック一覧を自動取得（0.4 秒スロットル）。
    /// 読むのは本体のイベントで印（`FOCUS_DIRTY`）が立っているときだけ（v0.9.9）。印が無ければ読まず、描き直しも頼まない
    fn poll_focus_and_auto_fetch(&mut self, ctx: &egui::Context) {
        // register() 時点でウィンドウは存在するが非表示のときは API を叩かない
        if !self.is_editor_window_visible() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return;
        }

        let now = ctx.input(|i| i.time);
        // 起動直後〜数秒はホスト初期化と重なるため API を叩かない
        if now < 2.0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return;
        }
        if !crate::FOCUS_DIRTY.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let since = now - self.last_focus_poll_secs;
        if since < FOCUS_POLL_INTERVAL_SECS {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(FOCUS_POLL_INTERVAL_SECS - since));
            return;
        }
        self.last_focus_poll_secs = now;
        crate::FOCUS_DIRTY.store(false, std::sync::atomic::Ordering::Release);

        let fingerprint = match edit_ops::catch_edit_panic(
            "focused_object_fingerprint",
            edit_ops::focused_object_fingerprint,
        ) {
            Ok(fp) => fp,
            Err(_) => {
                // 読めなかった（出力中など）。ステータスは汚さず、印を戻して後で読み直す
                crate::FOCUS_DIRTY.store(true, std::sync::atomic::Ordering::Release);
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(FOCUS_POLL_INTERVAL_SECS));
                return;
            }
        };

        match fingerprint {
            Some(key) => {
                if self.last_focus_fingerprint.as_ref() != Some(&key) {
                    self.last_focus_fingerprint = Some(key);
                    self.fetch_tracks_now_quiet(ctx);
                }
            }
            None => {
                if self.last_focus_fingerprint.take().is_some() {
                    let mut s = self.state.write();
                    s.apply_tracks.clear();
                    s.apply_track_index = 0;
                    s.track_overlay = None;
                    s.status_message =
                        "フォーカスなし - タイムラインでオブジェクトを選ぶとトラックを自動取得します"
                            .into();
                }
            }
        }
    }

    /// 自動取得用（ステータスを「トラック取得中」で騒がしくしない）。
    fn fetch_tracks_now_quiet(&mut self, ctx: &egui::Context) {
        let started = std::time::Instant::now();
        match edit_ops::catch_edit_panic("list_focused_object_tracks", edit_ops::list_focused_object_tracks)
        {
            Ok(tracks) => {
                let n = tracks.len();
                self.pending_fetched_tracks = Some(tracks);
                self.state.write().status_message =
                    format!("トラックを自動取得しました（{n} 件）");
            }
            Err(e) => {
                // 自動取得の失敗は一覧を空にしてメッセージのみ（手動「トラック取得」は従来どおり）
                self.state.write().apply_tracks.clear();
                self.pending_fetch_error = Some(e);
            }
        }
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            "auto fetch tracks done"
        );
        ctx.request_repaint();
    }

    /// 操作が一区切りついたフレームで、カーブの変化を Undo の 1 段として確定する。
    /// ドラッグ中・数値やテキストの入力中は確定しない（1 回の操作が 1 段になる）。
    fn commit_undo_if_idle(&mut self, ctx: &egui::Context) {
        let busy = self.dragging.is_some()
            || ctx.input(|i| i.pointer.any_down())
            || ctx.text_edit_focused();
        if !busy {
            self.state.write().commit_undo_step();
        }
    }

    /// Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z。
    ///
    /// 処理したキーは消費する。aviutl2-eframe は egui が使わなかったキーを本体へ転送するので、
    /// 消費しないと CE2 と本体の両方で元に戻すが走る。
    /// テキスト欄（Lua 欄・数値の直接入力）にフォーカスがある間は、その欄の文字の Undo に任せる。
    fn handle_undo_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() || self.dragging.is_some() {
            return;
        }
        let (undo, redo) = ctx.input_mut(|i| {
            // matches_logically は余分な Shift を無視するので、Shift 付きを先に判定する
            let redo = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z)
                || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
            let undo = !redo && i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            (undo, redo)
        });
        if undo {
            self.state.write().undo_edit();
        } else if redo {
            self.state.write().redo_edit();
        }
    }

    fn dismiss_menus_and_panels(&mut self, ctx: &egui::Context) {
        egui::Popup::close_all(ctx);
        self.state.write().search_expanded = false;
        self.presets_panel_open = false;
    }
}

impl eframe::App for CurveEditorHApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.run_deferred_api_work(ui.ctx());
        self.poll_focus_and_auto_fetch(ui.ctx());

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dismiss_menus_and_panels(ui.ctx());
        }

        self.handle_undo_shortcuts(ui.ctx());

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                // 幅が足りないときはタイトルを詰めて、右のトグルを落とさない。
                if ui.available_width() > 430.0 {
                    ui.heading("CurveEditor_H");
                } else {
                    ui.strong("CE2");
                }
                {
                    let mut s = self.state.write();
                    if ui
                        .add_enabled(s.can_undo(), egui::Button::new("↶"))
                        .on_hover_text(
                            "元に戻す (Ctrl+Z)\nカーブ編集のみ。CE2 のウィンドウを操作しているときに効く\n「このトラックに適用」の取り消しは AviUtl2 本体の元に戻すを使う",
                        )
                        .clicked()
                    {
                        s.undo_edit();
                    }
                    if ui
                        .add_enabled(s.can_redo(), egui::Button::new("↷"))
                        .on_hover_text("やり直す (Ctrl+Y / Ctrl+Shift+Z)\nカーブ編集のみ")
                        .clicked()
                    {
                        s.redo_edit();
                    }
                }
                ui.separator();
                if ui
                    .selectable_label(self.presets_panel_open, "プリセット")
                    .on_hover_text("左のプリセットパネル表示切替")
                    .clicked()
                {
                    self.presets_panel_open = !self.presets_panel_open;
                }
                if ui
                    .selectable_label(self.lua_panel_open, "Lua")
                    .on_hover_text("下の Lua 欄の表示切替。閉じるとその高さはグラフへ回る")
                    .clicked()
                {
                    self.lua_panel_open = !self.lua_panel_open;
                }
                if ui
                    .selectable_label(self.graph_focus_mode, "集中")
                    .on_hover_text("グラフ集中モード: 操作パネルと Lua 欄を畳んでグラフだけにする")
                    .clicked()
                {
                    self.graph_focus_mode = !self.graph_focus_mode;
                }
                if ui
                    .small_button("✕")
                    .on_hover_text("開いているメニュー・パネルを閉じる (Esc)")
                    .clicked()
                {
                    self.dismiss_menus_and_panels(ui.ctx());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⟳").on_hover_text("プリセット再読込").clicked() {
                        self.state.write().reload_presets();
                    }
                });
            });
        });

        if self.presets_panel_open {
            egui::Panel::left("presets")
                .default_size(140.0)
                .resizable(true)
                .show(ui, |ui| {
                    self.render_presets(ui);
                });
        }

        // 下から順に積み、CentralPanel（グラフ）を最後に置く。
        // 以前は CentralPanel を先に出していたため、ステータスバーと検索欄が
        // 中央パネルの後ろに回り込んでいた。
        egui::Panel::bottom("status").show(ui, |ui| {
            let msg = self.state.read().status_message.clone();
            ui.horizontal(|ui| {
                ui.label(&msg);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.small(format!("v{}", self.version));
                });
            });
        });

        if self.state.read().search_expanded {
            egui::Panel::bottom("search")
                .default_size(120.0)
                .resizable(true)
                .show(ui, |ui| {
                    self.render_search_panel(ui);
                });
        }

        // 左パネルを引いた後の幅で判定する（グラフと操作パネルの実効幅）。
        let layout = LayoutProfile::from_width(ui.available_width());

        // Lua 欄は下パネル。畳んだ高さはそのままグラフへ回る。
        if !self.graph_focus_mode && self.lua_panel_open {
            let state = Arc::clone(&self.state);
            let lua_focused = &mut self.lua_editor_focused;
            let mut open = true;
            egui::Panel::bottom("lua")
                .resizable(true)
                .default_size(if layout.compact { 150.0 } else { 180.0 })
                .min_size(64.0)
                .max_size((ui.available_height() * 0.5).max(120.0))
                .show_collapsible(ui, &mut open, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("lua_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut s = state.write();
                            Self::render_script_editor(ui, &mut s, layout, lua_focused);
                        });
                });
            // 境界を下端までドラッグしたときも閉じる（トグルと状態を合わせる）。
            self.lua_panel_open = open;
        } else {
            // Lua 欄を描かないフレームではフォーカス状態を落とす。
            // 立てたまま隠すと refresh_code_from_mode_unless() が永久に抑止される。
            self.lua_editor_focused = false;
        }

        // 操作パネル。境界のドラッグで高さを変えられる。
        if !self.graph_focus_mode {
            let max_h = (ui.available_height() * 0.6).max(140.0);
            egui::Panel::top("controls")
                .resizable(true)
                .default_size(if layout.compact { 210.0 } else { 230.0 })
                .min_size(48.0)
                .max_size(max_h)
                .show(ui, |ui| {
                    self.render_controls(ui);
                });
        }

        // 残りは全部グラフ。ウィンドウを広げた分もここが受け取る。
        egui::CentralPanel::default().show(ui, |ui| {
            self.render_plot_area(ui, layout);
        });

        self.commit_undo_if_idle(ui.ctx());
        self.sync_live_if_due(ui.ctx());
    }
}

impl CurveEditorHApp {
    fn render_presets(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("プリセット");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("✕")
                    .on_hover_text("パネルを閉じる")
                    .clicked()
                {
                    self.presets_panel_open = false;
                    egui::Popup::close_all(ui.ctx());
                }
            });
        });
        ui.separator();

        let mut categories: HashSet<String> = HashSet::new();
        {
            let s = self.state.read();
            for (name, _) in &s.presets {
                let (cat, _) = storage::category_of(name);
                categories.insert(if cat.is_empty() {
                    "その他".to_string()
                } else {
                    cat.to_string()
                });
            }
        }

        let mut cats: Vec<_> = categories.into_iter().collect();
        cats.sort();

        {
            let mut s = self.state.write();
            if s.selected_category.is_empty() {
                s.selected_category = cats.first().cloned().unwrap_or_else(|| "その他".to_string());
            }
        }

        egui::ScrollArea::vertical()
            .id_salt("preset_cats")
            .max_height(100.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for cat in &cats {
                    let selected = self.state.read().selected_category == *cat;
                    if ui.selectable_label(selected, cat).clicked() {
                        self.state.write().selected_category = cat.clone();
                    }
                }
            });

        ui.separator();
        ui.label("一覧");

        let selected_cat = self.state.read().selected_category.clone();
        let presets: Vec<(String, String)> = self
            .state
            .read()
            .presets
            .iter()
            .filter(|(name, _)| {
                let (cat, _) = storage::category_of(name);
                let display_cat = if cat.is_empty() { "その他" } else { cat };
                display_cat == selected_cat
            })
            .cloned()
            .collect();

        egui::ScrollArea::vertical()
            .id_salt("preset_list")
            .max_height(200.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (name, code) in &presets {
                    let (_, leaf) = storage::category_of(name);
                    if ui.button(leaf).clicked() {
                        let mut s = self.state.write();
                        s.preset_name = name.clone();
                        s.script_code = code.clone();
                        s.mode = CurveMode::Script;
                        s.status_message = format!("プリセット「{leaf}」を読み込みました");
                        self.presets_panel_open = false;
                        egui::Popup::close_all(ui.ctx());
                    }
                }
            });

        ui.separator();
        ui.heading("カーブライブラリ");
        let ids = self.state.read().curve_library.sorted_ids();
        if ids.is_empty() {
            ui.label("保存済み ID なし");
        } else {
            let lib_h = ui.available_height().max(120.0);
            egui::ScrollArea::vertical()
                .id_salt("curve_library")
                .max_height(lib_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for id in ids {
                        let selected = self.state.read().curve_id == id;
                        if ui.selectable_label(selected, format!("ID {id}")).clicked() {
                            let mut s = self.state.write();
                            s.set_curve_id(id);
                            s.load_curve_from_library(true);
                        }
                    }
                });
        }
    }

    fn render_search_panel(&mut self, ui: &mut egui::Ui) {
        let (message, detail, hits, expanded) = {
            let s = self.state.read();
            (s.search_message.clone(), s.search_detail.clone(), s.search_hits.clone(), s.search_expanded)
        };

        // 見出しの行にはボタンだけを並べ、件数と説明は下の行で折り返す
        // （1 行に詰めると狭いウィンドウで説明とボタンが切れる。v0.9.5 の 13 段目）
        ui.horizontal(|ui| {
            ui.heading("使用箇所検索");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("閉じる").clicked() {
                    self.state.write().search_expanded = false;
                }
                if ui.button("再検索").clicked() {
                    self.usage_search_now();
                }
            });
        });
        if !message.is_empty() {
            ui.add(egui::Label::new(&message).wrap());
        }
        if !detail.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(&detail).small().weak()).wrap());
        }

        if expanded {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, hit) in hits.iter().enumerate() {
                    if ui
                        .selectable_label(false, hit.format_line())
                        .on_hover_text("クリックでタイムライン上のオブジェクトをフォーカス")
                        .clicked()
                    {
                        let hit = self.state.write().focus_search_hit(i);
                        if let Some(hit) = hit {
                            self.focus_usage_hit_now(hit);
                        }
                    }
                }
            });
        }
    }

    fn render_script_editor(
        ui: &mut egui::Ui,
        s: &mut EditorState,
        layout: LayoutProfile,
        lua_focused: &mut bool,
    ) {
        let lua_applyable = matches!(
            s.mode,
            CurveMode::Bezier
                | CurveMode::MultiBezier
                | CurveMode::Spring
                | CurveMode::Bounce
                | CurveMode::Composite
        );

        ui.horizontal(|ui| {
            if layout.compact {
                ui.label("Lua");
            } else {
                ui.label("Lua コード (@カーブエディタ_H 用)");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if lua_applyable && ui.button("Luaから反映").clicked() {
                    if let Some(err) = s.try_apply_lua_edits() {
                        s.status_message = format!("Lua反映エラー: {err}");
                    } else {
                        s.refresh_code_from_mode();
                        s.status_message = "Lua から反映しました".to_string();
                    }
                }
                let lines = s.script_code.lines().count().max(1);
                ui.small(format!("{lines} 行"));
            });
        });

        let validation = script_editor::validate_lua_script(&s.script_code);
        if let Some(msg) = &validation {
            ui.colored_label(egui::Color32::from_rgb(255, 180, 80), msg);
        }
        if let Some(err) = &s.lua_parse_error {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), err);
        }

        let line_count = s.script_code.lines().count().max(if layout.narrow { 5 } else { 8 });
        let show_gutter = !layout.narrow;

        ui.horizontal_top(|ui| {
            if show_gutter {
                ui.vertical(|ui| {
                    ui.set_width(28.0);
                    for i in 1..=line_count {
                        ui.label(
                            egui::RichText::new(format!("{i:>2}"))
                                .monospace()
                                .size(11.0)
                                .color(egui::Color32::from_gray(120)),
                        );
                    }
                });
            }

            let text_edit = egui::TextEdit::multiline(&mut s.script_code)
                .desired_rows(line_count.min(if layout.compact { 6 } else { 8 }))
                .font(egui::TextStyle::Monospace)
                .code_editor()
                .lock_focus(true);
            let response = ui.add(text_edit);

            *lua_focused = response.has_focus();

            if response.lost_focus() && lua_applyable {
                if let Some(err) = s.try_apply_lua_edits() {
                    s.status_message = format!("Lua反映エラー: {err}");
                } else {
                    s.refresh_code_from_mode();
                    s.mark_live_dirty();
                }
            }
        });

        if !layout.narrow {
            ui.horizontal_wrapped(|ui| {
                ui.label("プレビュー:");
                for line in s.script_code.lines().take(3) {
                    let preview = script_editor::highlight_line(line);
                    ui.label(egui::RichText::new(preview).monospace().size(11.0));
                }
            });
        }
    }

    /// 操作パネル（モード / スロット / トラック適用 / プリセット / 数値入力）。
    /// グラフと Lua 欄は別パネルなので、ここは折りたたみ節に分けてスクロールさせ、
    /// グラフの高さを奪わないようにする。
    fn render_controls(&mut self, ui: &mut egui::Ui) {
        let layout = LayoutProfile::from_width(ui.available_width());
        if layout.compact {
            let style = ui.style_mut();
            style.spacing.item_spacing = egui::vec2(4.0, 3.0);
            style.spacing.button_padding = egui::vec2(4.0, 2.0);
        }

        let mut request_fetch_tracks = false;
        let mut request_apply_track = false;
        let mut request_usage_search = false;
        let mut request_overlay_refresh = false;
        let lua_focused = self.lua_editor_focused;

        egui::ScrollArea::vertical()
            .id_salt("controls_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
        {
            let mut s = self.state.write();

            if layout.compact {
                ui.horizontal(|ui| {
                    ui.small("モード");
                    // 畳んでも残る唯一の行なので、狭すぎない限りは正式名を出す。
                    let selected = if layout.narrow {
                        s.mode.short_label()
                    } else {
                        s.mode.label()
                    };
                    egui::ComboBox::from_id_salt("mode_select")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for mode in CurveMode::ALL {
                                let text = if layout.narrow {
                                    mode.short_label()
                                } else {
                                    mode.label()
                                };
                                if ui
                                    .selectable_label(s.mode == mode, text)
                                    .on_hover_text(mode.hint())
                                    .clicked()
                                    && s.mode != mode
                                {
                                    s.mode = mode;
                                    s.refresh_code_from_mode();
                                }
                            }
                        });
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label("モード:");
                    for mode in CurveMode::ALL {
                        if ui
                            .selectable_label(s.mode == mode, mode.label())
                            .on_hover_text(mode.hint())
                            .clicked()
                            && s.mode != mode
                        {
                            s.mode = mode;
                            s.refresh_code_from_mode();
                        }
                    }
                });
            }
            if !layout.narrow {
                ui.small(s.mode.hint());
            }

            ui.separator();

            let slot_header = if layout.compact {
                "スロット / ID"
            } else {
                "スロット / カーブライブラリ"
            };
            egui::CollapsingHeader::new(slot_header)
                .id_salt("controls_slot")
                .default_open(true)
                .show(ui, |ui| {

            // ID連携 ON: スロット＝カーブ ID を1コントロールに統合
            if s.slot_id_linked {
                ui.horizontal_wrapped(|ui| {
                    ui.label(if layout.compact {
                        "Slot/ID"
                    } else {
                        "スロット / ID:"
                    })
                    .on_hover_text("Live スロット番号とカーブライブラリ ID を共通化（ID連携 ON）");
                    if ui.button("◀").clicked() && s.curve_id > 1 {
                        let next = s.curve_id - 1;
                        s.set_curve_id(next);
                        s.load_curve_from_library(true);
                    }
                    let mut id = s.curve_id as i64;
                    if ui
                        .add(
                            egui::DragValue::new(&mut id)
                                .range(1..=MAX_CURVE_ID as i64)
                                .speed(0.5),
                        )
                        .changed()
                    {
                        s.set_curve_id(id.clamp(1, MAX_CURVE_ID as i64) as u32);
                    }
                    if ui.button("▶").clicked() && s.curve_id < MAX_CURVE_ID {
                        let next = s.curve_id + 1;
                        s.set_curve_id(next);
                        s.load_curve_from_library(true);
                    }
                    let live_was = s.live_enabled;
                    ui.checkbox(&mut s.live_enabled, "Live");
                    if !live_was && s.live_enabled {
                        s.mark_live_dirty();
                    }
                    ui.checkbox(&mut s.slot_id_linked, if layout.compact { "ID連" } else { "ID連携" })
                        .on_hover_text("ON: スロット番号＝カーブ ID。ID保存時に Live へ自動同期");
                    ui.small(format!("({})", s.curve_library.defined_count()));
                });
            } else if layout.compact {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Slot");
                        if ui.button("◀").clicked() && s.slot > 1 {
                            let next = s.slot - 1;
                            s.set_slot(next);
                        }
                        let mut slot = s.slot;
                        if ui
                            .add(egui::DragValue::new(&mut slot).range(1..=9999).speed(0.1))
                            .changed()
                        {
                            s.set_slot(slot);
                        }
                        if ui.button("▶").clicked() {
                            let next = s.slot + 1;
                            s.set_slot(next);
                        }
                    });
                    ui.horizontal(|ui| {
                        let live_was = s.live_enabled;
                        ui.checkbox(&mut s.live_enabled, "Live");
                        if !live_was && s.live_enabled {
                            s.mark_live_dirty();
                        }
                        ui.checkbox(&mut s.slot_id_linked, "ID連")
                            .on_hover_text("ON: スロット番号＝カーブ ID。ID保存時に Live へ自動同期");
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("ID");
                    if ui.button("◀").clicked() && s.curve_id > 1 {
                        let next = s.curve_id - 1;
                        s.set_curve_id(next);
                        s.load_curve_from_library(true);
                    }
                    let mut id = s.curve_id as i64;
                    if ui
                        .add(
                            egui::DragValue::new(&mut id)
                                .range(1..=MAX_CURVE_ID as i64)
                                .speed(0.5),
                        )
                        .changed()
                    {
                        s.set_curve_id(id.clamp(1, MAX_CURVE_ID as i64) as u32);
                    }
                    if ui.button("▶").clicked() && s.curve_id < MAX_CURVE_ID {
                        let next = s.curve_id + 1;
                        s.set_curve_id(next);
                        s.load_curve_from_library(true);
                    }
                    ui.small(format!("({})", s.curve_library.defined_count()));
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label("スロット:");
                    if ui.button("◀").clicked() && s.slot > 1 {
                        let next = s.slot - 1;
                        s.set_slot(next);
                    }
                    let mut slot = s.slot;
                    if ui
                        .add(egui::DragValue::new(&mut slot).range(1..=9999).speed(0.1))
                        .changed()
                    {
                        s.set_slot(slot);
                    }
                    if ui.button("▶").clicked() {
                        let next = s.slot + 1;
                        s.set_slot(next);
                    }
                    let live_was = s.live_enabled;
                    ui.checkbox(&mut s.live_enabled, "Live");
                    if !live_was && s.live_enabled {
                        s.mark_live_dirty();
                    }
                    ui.checkbox(&mut s.slot_id_linked, "ID連携")
                        .on_hover_text("ON: スロット番号＝カーブ ID。ID保存時に Live へ自動同期");
                });
                ui.horizontal(|ui| {
                    ui.label("カーブ ID:");
                    if ui.button("◀").clicked() && s.curve_id > 1 {
                        let next = s.curve_id - 1;
                        s.set_curve_id(next);
                        s.load_curve_from_library(true);
                    }
                    let mut id = s.curve_id as i64;
                    if ui
                        .add(
                            egui::DragValue::new(&mut id)
                                .range(1..=MAX_CURVE_ID as i64)
                                .speed(0.5),
                        )
                        .changed()
                    {
                        s.set_curve_id(id.clamp(1, MAX_CURVE_ID as i64) as u32);
                    }
                    if ui.button("▶").clicked() && s.curve_id < MAX_CURVE_ID {
                        let next = s.curve_id + 1;
                        s.set_curve_id(next);
                        s.load_curve_from_library(true);
                    }
                    ui.small(format!("({})", s.curve_library.defined_count()));
                });
            }

            ui.horizontal(|ui| {
                let (read, save, del, search) = if layout.compact {
                    ("読込", "保存", "削除", "検索")
                } else {
                    ("ID読込", "ID保存", "ID削除", "使用箇所")
                };
                if ui.button(read).on_hover_text("ライブラリから現在の ID を読み込む").clicked() {
                    s.load_curve_from_library(true);
                }
                if ui.button(save).on_hover_text("現在の編集内容をライブラリ ID に保存").clicked() {
                    s.save_curve_to_library();
                }
                if ui.button(del).on_hover_text("ライブラリから現在の ID を削除").clicked() {
                    s.delete_curve_from_library();
                }
                if ui
                    .button(search)
                    .on_hover_text("プロジェクト内の使用箇所を検索（今のシーンは編集中の状態、ほかのシーンは保存済みのファイルを読む）")
                    .clicked()
                {
                    request_usage_search = true;
                }
            });

                });

            let apply_header = if layout.compact {
                "トラック適用"
            } else {
                "トラックバーへ適用（フォーカスオブジェクト）"
            };
            egui::CollapsingHeader::new(apply_header)
                .id_salt("controls_apply")
                .default_open(true)
                .show(ui, |ui| {

            ui.small(edit_ops::format_apply_target(
                s.mode,
                &s.preset_name,
                s.effective_live_slot(),
            ));
            ui.horizontal_wrapped(|ui| {
                let fetch_label = if layout.compact { "取得" } else { "トラック取得" };
                if ui.button(fetch_label).on_hover_text("フォーカス中オブジェクトのトラック一覧を取得").clicked() {
                    request_fetch_tracks = true;
                }
                if !s.apply_tracks.is_empty() {
                    let labels: Vec<String> = s.apply_tracks.iter().map(|t| t.label.clone()).collect();
                    let short_labels: Vec<String> = labels
                        .iter()
                        .map(|l| {
                            if layout.compact {
                                truncate_display(l, 6)
                            } else {
                                l.clone()
                            }
                        })
                        .collect();
                    let track_changed = egui::ComboBox::from_id_salt("apply_track")
                        .width(if layout.compact { 120.0 } else { 180.0 })
                        .selected_text(
                            short_labels
                                .get(s.apply_track_index)
                                .cloned()
                                .unwrap_or_else(|| "（未選択）".into()),
                        )
                        .show_ui(ui, |ui| {
                            for (i, label) in labels.iter().enumerate() {
                                if ui.selectable_label(s.apply_track_index == i, label).clicked() {
                                    s.apply_track_index = i;
                                }
                            }
                        })
                        .response
                        .changed();
                    if track_changed {
                        request_overlay_refresh = true;
                        s.track_overlay = None;
                        s.track_overlay_message = "トラックオーバーレイ: 読込中\u{2026}".into();
                    }
                }
                let overlay_label = if layout.compact { "OL" } else { "オーバーレイ" };
                if ui
                    .checkbox(&mut s.track_overlay_enabled, overlay_label)
                    .on_hover_text("選択トラックの値域に編集中カーブを重ねて表示（トラック取得時の値を使用）")
                    .changed()
                    && s.track_overlay_enabled
                {
                    request_overlay_refresh = true;
                    s.clear_overlay_target_hint();
                }
                if ui
                    .small_button("更新")
                    .on_hover_text("キャッシュ済みトラック値でオーバーレイを再描画（API は呼びません）")
                    .clicked()
                {
                    request_overlay_refresh = true;
                }
                let apply_label = if layout.compact {
                    "適用"
                } else {
                    "このトラックに適用"
                };
                if ui
                    .button(apply_label)
                    .on_hover_text(
                        "Live スロットへ反映したうえで、選択トラックの移動方法をカーブエディタ_Hに設定",
                    )
                    .clicked()
                {
                    request_apply_track = true;
                }
                // Live スロットへの書き込みは適用と同じ文脈の操作なので、
                // プリセット節（既定で畳む）ではなくここに置く。
                let reflect_label = if layout.compact {
                    "反映"
                } else {
                    "スロットに反映"
                };
                if ui
                    .button(reflect_label)
                    .on_hover_text(
                        "編集中カーブを現在の Live スロットへ書き込む（トラックの移動方法は変えない）",
                    )
                    .clicked()
                {
                    if let Err(e) = s.apply_to_slot() {
                        s.status_message = format!("反映エラー: {e}");
                    }
                }
            });

                });

            egui::CollapsingHeader::new("プリセット")
                .id_salt("controls_preset")
                .default_open(false)
                .show(ui, |ui| {

            if layout.compact {
                ui.horizontal(|ui| {
                    ui.label("名:");
                    ui.add(
                        egui::TextEdit::singleline(&mut s.preset_name)
                            .desired_width(ui.available_width().min(140.0)),
                    );
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label("プリセット名:");
                    ui.text_edit_singleline(&mut s.preset_name);
                });
            }

            ui.horizontal_wrapped(|ui| {
                if ui.button("保存").clicked() {
                    let name = s.preset_name.trim().to_string();
                    if name.is_empty() {
                        s.status_message = "プリセット名を入力してください".to_string();
                    } else {
                        let code = s.current_code();
                        match storage::save_preset(&name, &code) {
                            Ok(()) => {
                                s.status_message = format!("プリセット「{name}」を保存しました");
                                s.reload_presets();
                            }
                            Err(e) => s.status_message = format!("保存エラー: {e}"),
                        }
                    }
                }
                if ui.button("削除").clicked() {
                    let name = s.preset_name.trim().to_string();
                    if !name.is_empty() {
                        match storage::delete_preset(&name) {
                            Ok(()) => {
                                s.status_message = format!("プリセット「{name}」を削除しました");
                                s.reload_presets();
                            }
                            Err(e) => s.status_message = format!("削除エラー: {e}"),
                        }
                    }
                }
            });

                });

            ui.separator();

            let mut edits = NumericEdits::default();

            // 数値入力はグラフで代替できるので既定は畳む。
            // アンカー列と区間種別が要るモード（マルチ／複合）だけ開いておく。
            // 開閉状態はモードごとに覚える（id_salt をモード名で分ける）。
            let params_open = matches!(s.mode, CurveMode::MultiBezier | CurveMode::Composite);
            let params_id = mode_to_str(s.mode);
            let params_header = if layout.compact {
                "数値入力"
            } else {
                "パラメータ（数値入力）"
            };
            egui::CollapsingHeader::new(params_header)
                .id_salt(("controls_params", params_id))
                .default_open(params_open)
                .show(ui, |ui| {

            match s.mode {
                CurveMode::Bezier => {
                    if layout.compact {
                        ui.small("制御点: グラフまたは下の数値で調整");
                    } else {
                        ui.label("制御点をグラフ上でドラッグ、または数値で調整");
                    }
                    if layout.compact {
                        edits.record(&coord_drag(ui, "y0", &mut s.bezier.y0));
                        edits.record(&coord_drag(ui, "x1", &mut s.bezier.x1));
                        edits.record(&coord_drag(ui, "y1", &mut s.bezier.y1));
                        edits.record(&coord_drag(ui, "x2", &mut s.bezier.x2));
                        edits.record(&coord_drag(ui, "y2", &mut s.bezier.y2));
                        edits.record(&coord_drag(ui, "y3", &mut s.bezier.y3));
                    } else {
                        ui.horizontal(|ui| {
                            edits.record(&coord_drag(ui, "y0", &mut s.bezier.y0));
                            edits.record(&coord_drag(ui, "y3", &mut s.bezier.y3));
                        });
                        ui.horizontal(|ui| {
                            edits.record(&coord_drag(ui, "x1", &mut s.bezier.x1));
                            edits.record(&coord_drag(ui, "y1", &mut s.bezier.y1));
                        });
                        ui.horizontal(|ui| {
                            edits.record(&coord_drag(ui, "x2", &mut s.bezier.x2));
                            edits.record(&coord_drag(ui, "y2", &mut s.bezier.y2));
                        });
                    }
                    s.refresh_code_from_mode_unless(lua_focused);
                }
                CurveMode::MultiBezier => {
                    if layout.compact {
                        ui.small("アンカー: ドラッグ / ダブルクリックで追加・削除");
                    } else {
                        ui.label("アンカーをドラッグで移動 / ダブルクリックで追加・削除（端点は削除不可）");
                    }
                    if ui.button(if layout.compact { "リセット" } else { "アンカーをリセット" }).clicked() {
                        s.multi_anchors = curves::default_multi_anchors();
                        s.selected_anchor = None;
                        s.refresh_code_from_mode_unless(lua_focused);
                    }
                    edits.merge(render_anchor_panel(ui, &mut s, AnchorPanelMode::Multi, layout));
                    s.refresh_code_from_mode_unless(lua_focused);
                }
                CurveMode::Spring => {
                    if layout.compact {
                        ui.small("減衰振動");
                    } else {
                        ui.label("減衰振動 (spring)");
                    }
                    edits.record(
                        &ui.add(egui::Slider::new(&mut s.spring.zeta, 0.0..=2.0).text("減衰比 ζ")),
                    );
                    edits.record(
                        &ui.add(
                            egui::Slider::new(&mut s.spring.omega, 1.0..=30.0)
                                .logarithmic(true)
                                .text("角周波数 ω"),
                        ),
                    );
                    s.refresh_code_from_mode_unless(lua_focused);
                }
                CurveMode::Bounce => {
                    if !layout.compact {
                        ui.label("バウンス・アウト（パラメータなし）");
                    }
                    s.refresh_code_from_mode_unless(lua_focused);
                }
                CurveMode::Composite => {
                    if layout.compact {
                        ui.small("アンカー編集 / 区間クリックで種別選択");
                    } else {
                        ui.label("アンカーをドラッグで移動 / ダブルクリックで追加・削除。区間をクリックして種別を選択");
                    }
                    if ui.button("リセット").clicked() {
                        s.composite = curves::CompositeCurve::default();
                        s.selected_segment = 0;
                        s.selected_anchor = None;
                        s.refresh_code_from_mode_unless(lua_focused);
                    }
                    edits.merge(render_anchor_panel(
                        ui,
                        &mut s,
                        AnchorPanelMode::Composite,
                        layout,
                    ));
                    let seg_count = s.composite.segments.len();
                    if seg_count > 0 {
                        s.selected_segment = s.selected_segment.min(seg_count - 1);
                        let sel = s.selected_segment;
                        let current_kind = s.composite.segments[sel].kind;
                        if layout.compact {
                            egui::ComboBox::from_id_salt("composite_kind")
                                .selected_text(format!("区間{}: {}", sel + 1, current_kind.short_label()))
                                .show_ui(ui, |ui| {
                                    for kind in SegmentKind::ALL {
                                        if ui
                                            .selectable_label(current_kind == kind, kind.label())
                                            .clicked()
                                            && current_kind != kind
                                        {
                                            s.composite.segments[sel].kind = kind;
                                            s.mark_live_dirty();
                                        }
                                    }
                                });
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(format!("選択区間: {}", sel + 1));
                                let mut picked = None;
                                for kind in SegmentKind::ALL {
                                    if ui
                                        .selectable_label(current_kind == kind, kind.label())
                                        .clicked()
                                        && current_kind != kind
                                    {
                                        picked = Some(kind);
                                    }
                                }
                                if let Some(kind) = picked {
                                    s.composite.segments[sel].kind = kind;
                                    s.mark_live_dirty();
                                }
                            });
                        }
                        let seg = &mut s.composite.segments[sel];
                        match seg.kind {
                            SegmentKind::Bezier => {
                                if layout.compact {
                                    edits.record(&coord_drag(ui, "x1", &mut seg.cx1));
                                    edits.record(&coord_drag(ui, "y1", &mut seg.cy1));
                                    edits.record(&coord_drag(ui, "x2", &mut seg.cx2));
                                    edits.record(&coord_drag(ui, "y2", &mut seg.cy2));
                                } else {
                                    ui.horizontal(|ui| {
                                        edits.record(&coord_drag(ui, "x1", &mut seg.cx1));
                                        edits.record(&coord_drag(ui, "y1", &mut seg.cy1));
                                    });
                                    ui.horizontal(|ui| {
                                        edits.record(&coord_drag(ui, "x2", &mut seg.cx2));
                                        edits.record(&coord_drag(ui, "y2", &mut seg.cy2));
                                    });
                                }
                            }
                            SegmentKind::Spring => {
                                edits.record(&ui.add(
                                    egui::Slider::new(&mut seg.zeta, 0.0..=2.0).text("減衰比 ζ"),
                                ));
                                edits.record(
                                    &ui.add(
                                        egui::Slider::new(&mut seg.omega, 1.0..=30.0)
                                            .logarithmic(true)
                                            .text("角周波数 ω"),
                                    ),
                                );
                            }
                            SegmentKind::Linear | SegmentKind::Bounce => {}
                        }
                    }
                    s.refresh_code_from_mode_unless(lua_focused);
                }
                CurveMode::Script => {}
            }

                });

            // 数値入力の節を畳んでいるフレームでも Lua 本文をモデルに追随させる。
            // （生成は code_cache で抑止されるので毎フレーム呼んでも安い）
            s.refresh_code_from_mode_unless(lua_focused);

            // 数値 UI の変更もグラフ操作と同じく Live 同期の対象にする。
            if edits.changed {
                s.mark_live_dirty();
            }
        }
            });

        if request_fetch_tracks {
            self.fetch_tracks_now(ui.ctx());
        }
        if request_apply_track {
            self.apply_track_now();
        }
        if request_usage_search {
            self.usage_search_now();
        }
        if request_overlay_refresh {
            self.pending_overlay_refresh = true;
        }
    }

    /// グラフ領域。中央パネルいっぱいを使う。
    /// 上端に「反転」と表示操作だけを置き、他の UI はここへ足さない
    /// （ここに 1 行足すたびにドラッグできる高さが減る）。
    fn render_plot_area(&mut self, ui: &mut egui::Ui, layout: LayoutProfile) {
        let flipped = {
            let mut s = self.state.write();
            render_curve_toolbar(ui, &mut s, layout)
        };

        // オーバーレイは下パネルとして先に場所を取る。
        // 直接 allocate するとグラフが残り全部を取った後になり、はみ出して切れる。
        if self.state.read().track_overlay_enabled {
            let state = Arc::clone(&self.state);
            egui::Panel::bottom("track_overlay")
                .resizable(false)
                .show(ui, |ui| {
                    let s = state.read();
                    paint_track_overlay(ui, &s);
                });
        }

        let plot_changed = {
            let mut s = self.state.write();
            interactive_plot(ui, &mut s, &mut self.dragging, layout)
        };

        if plot_changed || flipped {
            let mut s = self.state.write();
            s.refresh_code_from_mode_unless(self.lua_editor_focused);
            s.mark_live_dirty();
        }

        let _ = &self.handle;
    }

    /// Live スロットへの書き込み。すべてのパネルを描き終えてから 1 回だけ呼ぶ。
    fn sync_live_if_due(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let mut s = self.state.write();

        // 以前は force に live_dirty を渡していたが、live_needs_sync() が
        // live_dirty を前提にしている以上つねに true になり、スロットルが
        // 効いていなかった（ドラッグ中は毎フレーム 2 ファイルを read + write）。
        // 判定は「安い順」に並べる。live_needs_sync() は Lua を生成するので最後。
        if s.live_enabled && s.live_dirty {
            if s.live_sync_due(now, false) {
                if s.live_needs_sync() {
                    if let Err(e) = s.apply_to_slot() {
                        s.status_message = format!("Live同期エラー: {e}");
                    } else {
                        s.live_last_sync_secs = now;
                    }
                } else {
                    // 内容が同じなら書かずに dirty を落とす
                    s.live_dirty = false;
                }
            } else {
                // 間引いた分は次の期限で必ず書き切る（操作を止めても取り残さない）
                let wait = (LIVE_SYNC_INTERVAL_SECS - (now - s.live_last_sync_secs)).max(0.0);
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
            }
        }
    }
}

/// どの向きに反転するか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlipRequest {
    Horizontal,
    Vertical,
    /// 左右＋上下（180°回転）。端点を保ったまま In/Out が入れ替わる。
    Both,
}

impl FlipRequest {
    fn axes(self) -> &'static [curves::FlipAxis] {
        match self {
            FlipRequest::Horizontal => &[curves::FlipAxis::Horizontal],
            FlipRequest::Vertical => &[curves::FlipAxis::Vertical],
            FlipRequest::Both => &[curves::FlipAxis::Horizontal, curves::FlipAxis::Vertical],
        }
    }

    fn name(self) -> &'static str {
        match self {
            FlipRequest::Horizontal => "左右反転",
            FlipRequest::Vertical => "上下反転",
            FlipRequest::Both => "In/Out 入替（180°回転）",
        }
    }
}

/// 編集中のカーブを反転する。反転したら `true`。
fn apply_flip(s: &mut EditorState, req: FlipRequest) -> bool {
    if !curves::mode_supports_flip(s.mode) {
        return false;
    }
    let mut unflippable = 0usize;
    for &axis in req.axes() {
        match s.mode {
            CurveMode::Bezier => curves::flip_bezier(&mut s.bezier, axis),
            CurveMode::MultiBezier => curves::flip_anchors(&mut s.multi_anchors, axis),
            CurveMode::Composite => {
                unflippable = unflippable.max(curves::flip_composite(&mut s.composite, axis));
            }
            // mode_supports_flip で弾いてある
            CurveMode::Spring | CurveMode::Bounce | CurveMode::Script => {}
        }
    }
    if s.mode == CurveMode::Composite {
        curves::normalize_composite(&mut s.composite, &mut s.selected_segment);
    }
    s.selected_anchor = None;
    s.status_message = if unflippable > 0 {
        format!(
            "{}: 振動・バウンス区間 {} 件は形を折り返せないので元のままです",
            req.name(),
            unflippable
        )
    } else {
        format!("{}しました", req.name())
    };
    true
}

const WHEEL_HINT: &str = "ホイール: 上下移動
Ctrl+ホイール: 縦ズーム
Shift+ホイール: 左右移動
Ctrl+Shift+ホイール: 横ズーム
右ドラッグ: パン
左ドラッグ: ハンドル移動
Shift+左ドラッグ: グリッドの交点にスナップ
ダブルクリック: アンカー追加・削除（マルチ／複合。Shift で交点に追加）";

/// グラフ上部のツールバー。カーブの反転と表示操作だけを置く。
/// ここに 1 行足すたびにドラッグできる高さが減るので、他の UI は入れない。
/// 反転したら `true`（呼び出し側で Lua 再生成と Live 同期を行う）。
fn render_curve_toolbar(ui: &mut egui::Ui, s: &mut EditorState, layout: LayoutProfile) -> bool {
    let supported = curves::mode_supports_flip(s.mode);
    let mut request: Option<FlipRequest> = None;

    ui.horizontal(|ui| {
        ui.small("反転:");
        let (h_label, v_label, b_label) = if layout.compact {
            ("左右", "上下", "In/Out")
        } else {
            ("左右反転", "上下反転", "In/Out入替")
        };
        let disabled_hint = "このモードは反転できません（振動・バウンス・スクリプトは形をパラメータで折り返せない）";

        if ui
            .add_enabled(supported, egui::Button::new(h_label))
            .on_hover_text("時間を反転する（反転後の t の値が元の 1-t の値になる）。始点と終点の値が入れ替わる")
            .on_disabled_hover_text(disabled_hint)
            .clicked()
        {
            request = Some(FlipRequest::Horizontal);
        }
        if ui
            .add_enabled(supported, egui::Button::new(v_label))
            .on_hover_text("値を上下に折り返す（始点と終点の中間を軸にする）")
            .on_disabled_hover_text(disabled_hint)
            .clicked()
        {
            request = Some(FlipRequest::Vertical);
        }
        if ui
            .add_enabled(supported, egui::Button::new(b_label))
            .on_hover_text("左右と上下をまとめて反転（180°回転）。始点・終点はそのままで、イーズイン⇄イーズアウトが入れ替わる")
            .on_disabled_hover_text(disabled_hint)
            .clicked()
        {
            request = Some(FlipRequest::Both);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(if layout.compact { "表示" } else { "表示リセット" })
                .on_hover_text("ズーム・パンを初期状態に戻す")
                .clicked()
            {
                s.view.reset();
            }
            ui.small("操作?").on_hover_text(WHEEL_HINT);
            ui.add(
                egui::DragValue::new(&mut s.grid_divisions)
                    .range(GRID_DIVISIONS_MIN..=GRID_DIVISIONS_MAX)
                    .speed(0.2)
                    .prefix(if layout.compact { "格子 " } else { "グリッド " }),
            )
            .on_hover_text(
                "グリッドの分割数（縦横共通）。ドラッグまたはダブルクリックで入力\nShift を押しながらハンドルをドラッグすると交点にスナップ",
            );
        });
    });

    match request {
        Some(req) => apply_flip(s, req),
        None => false,
    }
}

fn interactive_plot(
    ui: &mut egui::Ui,
    s: &mut EditorState,
    dragging: &mut Option<DragTarget>,
    layout: LayoutProfile,
) -> bool {
        // 高さは中央パネルの残り全部。ウィンドウを広げた分はここが受け取る。
        let min_height = if layout.narrow { 120.0 } else { 160.0 };
        let height = ui.available_height().max(min_height);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, egui::Color32::from_gray(28));

        let plot_rect = rect.shrink(10.0);

        // ホイール: パン / Ctrl: ズーム / Shift: 横パン / Ctrl+Shift: 横ズーム。右ドラッグでもパン。
        if response.hovered() {
            let (scroll, zoom_2d, mods, hover_pos) = ui.input(|i| {
                (
                    i.smooth_scroll_delta,
                    i.zoom_delta_2d(),
                    i.modifiers,
                    i.pointer.hover_pos(),
                )
            });
            if handle_plot_wheel(&mut s.view, plot_rect, hover_pos, scroll, zoom_2d, mods) {
                ui.ctx().input_mut(|i| {
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                });
            }
        }
        if response.dragged_by(egui::PointerButton::Secondary) {
            let d = response.drag_delta();
            s.view.pan_y((d.y / plot_rect.height().max(1.0)) as f64);
            // 右ドラッグ左右は「掴んで動かす」感覚に合わせ符号を反転
            s.view.pan_x((-d.x / plot_rect.width().max(1.0)) as f64);
        }

        let t_min = s.view.t_min;
        let t_max = s.view.t_max;
        let y_min = s.view.y_min;
        let y_max = s.view.y_max;
        let to_pos = |t: f64, y: f64| -> egui::Pos2 {
            let tn = ((t - t_min) / (t_max - t_min)).clamp(-0.2, 1.2) as f32;
            let x = plot_rect.left() + tn * plot_rect.width();
            let yn = ((y - y_min) / (y_max - y_min)).clamp(-0.2, 1.2) as f32;
            let yp = plot_rect.bottom() - yn * plot_rect.height();
            egui::pos2(x, yp)
        };
        let from_pos = |pos: egui::Pos2| -> (f64, f64) {
            let tn = ((pos.x - plot_rect.left()) / plot_rect.width()).clamp(0.0, 1.0) as f64;
            let t = t_min + tn * (t_max - t_min);
            let yn = 1.0 - (pos.y - plot_rect.top()) as f64 / plot_rect.height() as f64;
            let y = y_min + yn * (y_max - y_min);
            (t, y)
        };

        // 分割線 細線 / 4 等分線と端 太線 / 中央 緑（縦横）。分割数 20 で従来の 5%/25%/50%。
        // X 方向は t∈[0,1] のみ（0未満・1超過の領域には描かない）。
        let x_at = |t: f64| -> f32 {
            let tn = ((t - t_min) / (t_max - t_min)) as f32;
            plot_rect.left() + tn * plot_rect.width()
        };
        let y_at = |y: f64| -> f32 {
            let yn = ((y - y_min) / (y_max - y_min)) as f32;
            plot_rect.bottom() - yn * plot_rect.height()
        };
        let grid_x0 = x_at(0.0).clamp(plot_rect.left(), plot_rect.right());
        let grid_x1 = x_at(1.0).clamp(plot_rect.left(), plot_rect.right());
        let (grid_left, grid_right) = if grid_x0 <= grid_x1 {
            (grid_x0, grid_x1)
        } else {
            (grid_x1, grid_x0)
        };

        let divisions = s.grid_divisions.clamp(GRID_DIVISIONS_MIN, GRID_DIVISIONS_MAX);
        let n = divisions as i64;
        let step = 1.0 / divisions as f64;
        // 線の種類: 中央（緑）/ 端と 4 等分（太）/ 分割（細）/ 0〜1 の外（ごく薄）
        let line_kind = |i: i64| -> u8 {
            if i < 0 || i > n {
                3
            } else if n % 2 == 0 && i == n / 2 {
                0
            } else if i == 0 || i == n || (n % 4 == 0 && i % (n / 4) == 0) {
                1
            } else {
                2
            }
        };
        let stroke_of = |kind: u8| match kind {
            0 => egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 180, 100)),
            1 => egui::Stroke::new(1.2, egui::Color32::from_gray(70)),
            2 => egui::Stroke::new(0.5, egui::Color32::from_gray(45)),
            _ => egui::Stroke::new(0.5, egui::Color32::from_gray(36)),
        };
        // 細線同士が詰まりすぎたら補助線（細・ごく薄）は省く。スナップはそのまま効く。
        let spacing_x = plot_rect.width() * (step / (t_max - t_min)) as f32;
        let spacing_y = plot_rect.height() * (step / (y_max - y_min)) as f32;

        // 水平線: 表示中の y 範囲すべて。ただし t∈[0,1] の帯だけ（X の 0未満・1超過には伸ばさない）
        if grid_right > grid_left + 0.5 {
            let j_lo = (y_min * divisions as f64).floor() as i64;
            let j_hi = (y_max * divisions as f64).ceil() as i64;
            for j in j_lo..=j_hi {
                let kind = line_kind(j);
                if kind >= 2 && spacing_y < GRID_MIN_SPACING_PX {
                    continue;
                }
                let yp = y_at(j as f64 * step);
                if yp >= plot_rect.top() - 0.5 && yp <= plot_rect.bottom() + 0.5 {
                    painter.line_segment(
                        [egui::pos2(grid_left, yp), egui::pos2(grid_right, yp)],
                        stroke_of(kind),
                    );
                }
            }
        }

        // 垂直線: t∈[0,1] かつ表示領域内のときだけ
        for i in 0..=n {
            let kind = line_kind(i);
            if kind >= 2 && spacing_x < GRID_MIN_SPACING_PX {
                continue;
            }
            let xp = x_at(i as f64 * step);
            if xp >= plot_rect.left() - 0.5 && xp <= plot_rect.right() + 0.5 {
                painter.line_segment(
                    [
                        egui::pos2(xp, plot_rect.top()),
                        egui::pos2(xp, plot_rect.bottom()),
                    ],
                    stroke_of(kind),
                );
            }
        }

        // 標準(複合)モードは区間の境界線を薄く表示する。
        if s.mode == CurveMode::Composite {
            for a in s.composite.anchors.iter().skip(1).take(s.composite.anchors.len().saturating_sub(2)) {
                let p = to_pos(a.t, y_min);
                let p2 = to_pos(a.t, y_max);
                painter.line_segment([p, p2], egui::Stroke::new(1.0, egui::Color32::from_gray(55)));
            }
        }

        let (points, script_preview_label) = curves::sample_curve_with_script(
            s.mode,
            &s.bezier,
            &s.spring,
            &s.multi_anchors,
            &s.composite,
            &s.script_code,
            120,
        );
        if points.len() >= 2 {
            let line_points: Vec<egui::Pos2> = points.iter().map(|[t, y]| to_pos(*t, *y)).collect();
            painter.add(egui::Shape::line(
                line_points,
                egui::Stroke::new(2.5, egui::Color32::from_rgb(100, 180, 255)),
            ));
        }
        if s.mode == CurveMode::Script {
            let note = match script_preview_label {
                Some(name) => format!("近似: {name}"),
                None => "プレビュー不可".to_string(),
            };
            painter.text(
                egui::pos2(plot_rect.left() + 4.0, plot_rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                note,
                egui::FontId::proportional(11.0),
                egui::Color32::from_gray(160),
            );
        }

        let mut changed = false;
        let pointer = ui.input(|i| i.pointer.clone());
        // Shift を押している間はグリッドの交点へ吸着させる（ドラッグ・ダブルクリック追加）
        let snap = ui.input(|i| i.modifiers.shift);
        let snapped = |(t, y): (f64, f64)| -> (f64, f64) {
            if snap {
                (snap_to_grid(t, divisions), snap_to_grid(y, divisions))
            } else {
                (t, y)
            }
        };

        if response.drag_started_by(egui::PointerButton::Primary) && dragging.is_none() {
            if let Some(pos) = pointer.interact_pos() {
                *dragging = hit_test(s, pos, &to_pos);
                if let Some(target) = *dragging {
                    match target {
                        DragTarget::Anchor(idx) => s.selected_anchor = Some(idx),
                        DragTarget::CompositeAnchor(idx) => s.selected_anchor = Some(idx),
                        _ => {}
                    }
                }
            }
        }

        if let Some(target) = *dragging {
            if pointer.primary_down() {
                if let Some(pos) = pointer.interact_pos() {
                    let (mut t, y) = snapped(from_pos(pos));
                    match target {
                        DragTarget::BezierStart => {
                            s.bezier.y0 = y;
                            changed = true;
                        }
                        DragTarget::BezierCp1 => {
                            s.bezier.x1 = t;
                            s.bezier.y1 = y;
                            changed = true;
                        }
                        DragTarget::BezierCp2 => {
                            s.bezier.x2 = t;
                            s.bezier.y2 = y;
                            changed = true;
                        }
                        DragTarget::BezierEnd => {
                            s.bezier.y3 = y;
                            changed = true;
                        }
                        DragTarget::Anchor(idx) => {
                            let len = s.multi_anchors.len();
                            let is_endpoint = idx == 0 || idx + 1 == len;
                            if is_endpoint {
                                // 端点は t 固定。sort_anchors による y 潰れを防ぐ
                                s.multi_anchors[idx].t = if idx == 0 { 0.0 } else { 1.0 };
                                s.multi_anchors[idx].y = y;
                            } else {
                                t = t.clamp(
                                    s.multi_anchors[idx - 1].t + 0.02,
                                    s.multi_anchors[idx + 1].t - 0.02,
                                );
                                s.multi_anchors[idx].t = t;
                                s.multi_anchors[idx].y = y;
                                curves::sort_anchors(&mut s.multi_anchors);
                            }
                            changed = true;
                        }
                        DragTarget::CompositeAnchor(idx) => {
                            let len = s.composite.anchors.len();
                            let is_endpoint = idx == 0 || idx + 1 == len;
                            if is_endpoint {
                                s.composite.anchors[idx].t = if idx == 0 { 0.0 } else { 1.0 };
                                s.composite.anchors[idx].y = y;
                            } else {
                                s.composite.anchors[idx].t = t;
                                s.composite.anchors[idx].y = y;
                                s.composite.clamp_anchor(idx);
                            }
                            changed = true;
                        }
                        DragTarget::CompositeCp1(seg_idx) => {
                            if let (Some(a0), Some(a1)) = (
                                s.composite.anchors.get(seg_idx).copied(),
                                s.composite.anchors.get(seg_idx + 1).copied(),
                            ) {
                                let (cx, cy) = curves::composite_cp_from_world(a0, a1, t, y);
                                let seg = &mut s.composite.segments[seg_idx];
                                seg.cx1 = cx;
                                seg.cy1 = cy;
                                changed = true;
                            }
                        }
                        DragTarget::CompositeCp2(seg_idx) => {
                            if let (Some(a0), Some(a1)) = (
                                s.composite.anchors.get(seg_idx).copied(),
                                s.composite.anchors.get(seg_idx + 1).copied(),
                            ) {
                                let (cx, cy) = curves::composite_cp_from_world(a0, a1, t, y);
                                let seg = &mut s.composite.segments[seg_idx];
                                seg.cx2 = cx;
                                seg.cy2 = cy;
                                changed = true;
                            }
                        }
                    }
                }
            } else {
                *dragging = None;
            }
        }

        if response.double_clicked() && s.mode == CurveMode::MultiBezier {
            if let Some(pos) = pointer.interact_pos() {
                let (t, y) = snapped(from_pos(pos));
                if let Some(idx) = hit_anchor_index(&s.multi_anchors, pos, &to_pos) {
                    if idx > 0 && idx + 1 < s.multi_anchors.len() {
                        s.multi_anchors.remove(idx);
                        changed = true;
                    }
                } else if t > 0.05 && t < 0.95 {
                    s.multi_anchors.push(Anchor::new(t, y));
                    curves::sort_anchors(&mut s.multi_anchors);
                    changed = true;
                }
            }
        }

        if response.double_clicked() && s.mode == CurveMode::Composite {
            if let Some(pos) = pointer.interact_pos() {
                let (t, y) = snapped(from_pos(pos));
                if let Some(idx) = hit_anchor_index(&s.composite.anchors, pos, &to_pos) {
                    s.composite.remove_anchor(idx);
                    changed = true;
                } else if t > 0.02 && t < 0.98 {
                    s.composite.insert_anchor_at(t, y);
                    changed = true;
                }
            }
        }

        // 複合モード: 区間の線をクリックすると選択区間が切り替わる。
        if s.mode == CurveMode::Composite
            && response.clicked()
            && dragging.is_none()
        {
            if let Some(pos) = pointer.interact_pos() {
                if let Some(idx) = hit_anchor_index(&s.composite.anchors, pos, &to_pos) {
                    s.selected_anchor = Some(idx);
                } else {
                    let (t, _) = from_pos(pos);
                    s.selected_segment = s.composite.segment_index_for(t);
                }
            }
        }

        if response.clicked() && dragging.is_none() && s.mode == CurveMode::MultiBezier {
            if let Some(pos) = pointer.interact_pos() {
                if let Some(idx) = hit_anchor_index(&s.multi_anchors, pos, &to_pos) {
                    s.selected_anchor = Some(idx);
                }
            }
        }

        draw_handles(&painter, s, &to_pos, *dragging);

        changed
}

fn hit_test(
    s: &EditorState,
    pos: egui::Pos2,
    to_pos: &impl Fn(f64, f64) -> egui::Pos2,
) -> Option<DragTarget> {
    match s.mode {
        CurveMode::Bezier => {
            let p0 = to_pos(0.0, s.bezier.y0);
            let p1 = to_pos(s.bezier.x1, s.bezier.y1);
            let p2 = to_pos(s.bezier.x2, s.bezier.y2);
            let p3 = to_pos(1.0, s.bezier.y3);
            // 制御点を端点より優先（重なったときに掴めるように）
            if pos.distance(p1) < HANDLE_PICK_RADIUS {
                return Some(DragTarget::BezierCp1);
            }
            if pos.distance(p2) < HANDLE_PICK_RADIUS {
                return Some(DragTarget::BezierCp2);
            }
            if pos.distance(p0) < HANDLE_PICK_RADIUS {
                return Some(DragTarget::BezierStart);
            }
            if pos.distance(p3) < HANDLE_PICK_RADIUS {
                return Some(DragTarget::BezierEnd);
            }
        }
        CurveMode::MultiBezier => {
            if let Some(idx) = hit_anchor_index(&s.multi_anchors, pos, to_pos) {
                return Some(DragTarget::Anchor(idx));
            }
        }
        CurveMode::Composite => {
            // 制御点をアンカーより先に判定（dy≈0 で重なりやすい）
            let seg_idx = s.selected_segment;
            if let Some(seg) = s.composite.segments.get(seg_idx) {
                if seg.kind == SegmentKind::Bezier {
                    if let (Some(a0), Some(a1)) = (
                        s.composite.anchors.get(seg_idx).copied(),
                        s.composite.anchors.get(seg_idx + 1).copied(),
                    ) {
                        let (t1, y1) = curves::composite_cp_world(a0, a1, seg.cx1, seg.cy1);
                        let (t2, y2) = curves::composite_cp_world(a0, a1, seg.cx2, seg.cy2);
                        let cp1 = to_pos(t1, y1);
                        let cp2 = to_pos(t2, y2);
                        if pos.distance(cp1) < HANDLE_PICK_RADIUS {
                            return Some(DragTarget::CompositeCp1(seg_idx));
                        }
                        if pos.distance(cp2) < HANDLE_PICK_RADIUS {
                            return Some(DragTarget::CompositeCp2(seg_idx));
                        }
                    }
                }
            }
            if let Some(idx) = hit_anchor_index(&s.composite.anchors, pos, to_pos) {
                return Some(DragTarget::CompositeAnchor(idx));
            }
        }
        _ => {}
    }
    None
}

fn hit_anchor_index(
    anchors: &[Anchor],
    pos: egui::Pos2,
    to_pos: &impl Fn(f64, f64) -> egui::Pos2,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, a) in anchors.iter().enumerate() {
        let p = to_pos(a.t, a.y);
        let d = pos.distance(p);
        if d < HANDLE_PICK_RADIUS {
            if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
    }
    best.map(|(i, _)| i)
}

fn draw_handles(
    painter: &egui::Painter,
    s: &EditorState,
    to_pos: &impl Fn(f64, f64) -> egui::Pos2,
    active: Option<DragTarget>,
) {
    let stroke = |active_flag: bool| {
        egui::Stroke::new(
            if active_flag { 2.5 } else { 1.5 },
            if active_flag {
                egui::Color32::from_rgb(255, 200, 80)
            } else {
                egui::Color32::from_gray(140)
            },
        )
    };

    match s.mode {
        CurveMode::Bezier => {
            let p0 = to_pos(0.0, s.bezier.y0);
            let p3 = to_pos(1.0, s.bezier.y3);
            let p1 = to_pos(s.bezier.x1, s.bezier.y1);
            let p2 = to_pos(s.bezier.x2, s.bezier.y2);
            painter.line_segment([p0, p1], stroke(false));
            painter.line_segment([p3, p2], stroke(false));
            draw_knob(
                painter,
                p0,
                egui::Color32::from_rgb(180, 180, 180),
                active == Some(DragTarget::BezierStart),
            );
            draw_knob(
                painter,
                p3,
                egui::Color32::from_rgb(180, 180, 180),
                active == Some(DragTarget::BezierEnd),
            );
            draw_knob(
                painter,
                p1,
                egui::Color32::from_rgb(255, 140, 60),
                active == Some(DragTarget::BezierCp1),
            );
            draw_knob(
                painter,
                p2,
                egui::Color32::from_rgb(255, 140, 60),
                active == Some(DragTarget::BezierCp2),
            );
        }
        CurveMode::MultiBezier => {
            for (i, a) in s.multi_anchors.iter().enumerate() {
                let p = to_pos(a.t, a.y);
                let is_endpoint = i == 0 || i + 1 == s.multi_anchors.len();
                let is_active = active == Some(DragTarget::Anchor(i));
                let is_selected = s.selected_anchor == Some(i);
                let color = if is_active || is_selected {
                    egui::Color32::from_rgb(255, 200, 80)
                } else if is_endpoint {
                    egui::Color32::from_rgb(180, 180, 180)
                } else {
                    egui::Color32::from_rgb(120, 220, 140)
                };
                draw_knob(
                    painter,
                    p,
                    color,
                    is_active || is_selected,
                );
            }
        }
        CurveMode::Composite => {
            let seg_idx = s.selected_segment;
            if let Some(seg) = s.composite.segments.get(seg_idx) {
                if seg.kind == SegmentKind::Bezier {
                    if let (Some(a0), Some(a1)) = (
                        s.composite.anchors.get(seg_idx),
                        s.composite.anchors.get(seg_idx + 1),
                    ) {
                        let p0 = to_pos(a0.t, a0.y);
                        let p1 = to_pos(a1.t, a1.y);
                        let (t1, y1) = curves::composite_cp_world(*a0, *a1, seg.cx1, seg.cy1);
                        let (t2, y2) = curves::composite_cp_world(*a0, *a1, seg.cx2, seg.cy2);
                        let cp1 = to_pos(t1, y1);
                        let cp2 = to_pos(t2, y2);
                        painter.line_segment([p0, cp1], stroke(false));
                        painter.line_segment([p1, cp2], stroke(false));
                        draw_knob(
                            painter,
                            cp1,
                            egui::Color32::from_rgb(255, 140, 60),
                            active == Some(DragTarget::CompositeCp1(seg_idx)),
                        );
                        draw_knob(
                            painter,
                            cp2,
                            egui::Color32::from_rgb(255, 140, 60),
                            active == Some(DragTarget::CompositeCp2(seg_idx)),
                        );
                    }
                }
            }
            for (i, a) in s.composite.anchors.iter().enumerate() {
                let p = to_pos(a.t, a.y);
                let is_endpoint = i == 0 || i + 1 == s.composite.anchors.len();
                let is_selected_boundary = i == seg_idx || i == seg_idx + 1;
                let is_active = active == Some(DragTarget::CompositeAnchor(i));
                let is_selected = s.selected_anchor == Some(i);
                let color = if is_active || is_selected {
                    egui::Color32::from_rgb(255, 200, 80)
                } else if is_endpoint {
                    egui::Color32::from_rgb(180, 180, 180)
                } else if is_selected_boundary {
                    egui::Color32::from_rgb(255, 200, 80)
                } else {
                    egui::Color32::from_rgb(120, 220, 140)
                };
                draw_knob(
                    painter,
                    p,
                    color,
                    is_active || is_selected,
                );
            }
        }
        _ => {}
    }
}

fn draw_knob(painter: &egui::Painter, pos: egui::Pos2, fill: egui::Color32, active: bool) {
    let r = if active {
        HANDLE_RADIUS + 2.0
    } else {
        HANDLE_RADIUS
    };
    painter.circle_filled(pos, r, fill);
    painter.circle_stroke(
        pos,
        r,
        egui::Stroke::new(1.5, egui::Color32::from_gray(30)),
    );
}

fn paint_track_overlay(ui: &mut egui::Ui, s: &EditorState) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("トラックオーバーレイ");
        if !s.track_overlay_message.is_empty() {
            ui.label(
                egui::RichText::new(&s.track_overlay_message)
                    .small()
                    .color(egui::Color32::from_gray(170)),
            );
        }
    });

    let Some(overlay) = &s.track_overlay else {
        return;
    };

    let height = 140.0;
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(24, 28, 36));

    let plot_rect = rect.shrink(10.0);
    let y_min = overlay.start_value.min(overlay.end_value);
    let y_max = overlay.start_value.max(overlay.end_value);
    let margin = (y_max - y_min).abs().max(1.0) * 0.15;
    let y_lo = y_min - margin;
    let y_hi = y_max + margin;

    let to_pos = |t: f64, y: f64| -> egui::Pos2 {
        let x = plot_rect.left() + (t as f32) * plot_rect.width();
        let yn = ((y - y_lo) / (y_hi - y_lo)).clamp(0.0, 1.0) as f32;
        let yp = plot_rect.bottom() - yn * plot_rect.height();
        egui::pos2(x, yp)
    };

    for &y in &[overlay.start_value, overlay.end_value] {
        let yp = to_pos(0.0, y).y;
        painter.line_segment(
            [
                egui::pos2(plot_rect.left(), yp),
                egui::pos2(plot_rect.right(), yp),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
        );
    }

    if overlay.actual_samples.len() >= 2 {
        let actual: Vec<egui::Pos2> = overlay
            .actual_samples
            .iter()
            .map(|(t, y)| to_pos(*t, *y))
            .collect();
        painter.add(egui::Shape::line(
            actual,
            egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(160, 160, 160, 180)),
        ));
    }

    let preview = track_overlay::preview_samples(
        s.mode,
        &s.bezier,
        &s.spring,
        &s.multi_anchors,
        &s.composite,
        &s.script_code,
        overlay.start_value,
        overlay.end_value,
        track_overlay::OVERLAY_SAMPLE_STEPS,
    );
    if preview.len() >= 2 {
        let preview_points: Vec<egui::Pos2> =
            preview.iter().map(|(t, y)| to_pos(*t, *y)).collect();
        let fill: Vec<egui::Pos2> = {
            let mut poly = preview_points.clone();
            poly.push(egui::pos2(plot_rect.right(), plot_rect.bottom()));
            poly.push(egui::pos2(plot_rect.left(), plot_rect.bottom()));
            poly
        };
        painter.add(egui::Shape::convex_polygon(
            fill,
            egui::Color32::from_rgba_unmultiplied(255, 160, 60, 35),
            egui::Stroke::NONE,
        ));
        painter.add(egui::Shape::line(
            preview_points,
            egui::Stroke::new(2.5, egui::Color32::from_rgba_unmultiplied(255, 160, 60, 220)),
        ));
    }

    painter.text(
        egui::pos2(plot_rect.left() + 4.0, plot_rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        "灰: 現在のトラック / 橙: 編集中カーブ",
        egui::FontId::proportional(11.0),
        egui::Color32::from_gray(140),
    );
    if let Some(mode) = &overlay.motion_mode {
        painter.text(
            egui::pos2(plot_rect.right() - 4.0, plot_rect.top() + 2.0),
            egui::Align2::RIGHT_TOP,
            mode,
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(140),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_grid_rounds_to_nearest_intersection() {
        assert!((snap_to_grid(0.26, 20) - 0.25).abs() < 1e-12);
        assert!((snap_to_grid(0.274, 20) - 0.25).abs() < 1e-12);
        assert!((snap_to_grid(0.276, 20) - 0.30).abs() < 1e-12);
        // 0〜1 の外（オーバーシュート）も同じ刻みで吸着する
        assert!((snap_to_grid(1.12, 10) - 1.1).abs() < 1e-12);
        assert!((snap_to_grid(-0.13, 10) + 0.1).abs() < 1e-12);
        // 分割数 0 でも 0 除算しない
        assert!(snap_to_grid(0.4, 0).is_finite());
    }

    /// 1 回の操作が 1 段になり、Undo / Redo で行き来できる（EditorState 経由）。
    #[test]
    fn editor_state_undo_redo() {
        let mut s = EditorState::default();
        let original_y1 = s.bezier.y1;

        // ドラッグで制御点を動かした（途中のフレームでは確定しない想定）
        s.bezier.y1 = 0.9;
        s.refresh_code_from_mode();
        assert!(s.can_undo(), "未確定の変更も戻せる");
        assert!(s.commit_undo_step());
        assert!(!s.commit_undo_step(), "変化が無ければ段を増やさない");

        assert!(s.undo_edit());
        assert!((s.bezier.y1 - original_y1).abs() < 1e-12);
        assert!(s.can_redo());

        assert!(s.redo_edit());
        assert!((s.bezier.y1 - 0.9).abs() < 1e-12);

        // 表示（ズーム・パン）だけの変更は段にしない
        s.view.zoom_y(0.5, 0.5);
        assert!(!s.commit_undo_step());
    }
}
