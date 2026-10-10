//! EditHandle 経由のトラックバー適用・オブジェクトフォーカス

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::LazyLock;

use aviutl2::generic::{
    EditHandleError, EditSection, EffectItemInfo, EffectItemType, GlobalEditHandle, ObjectHandle,
    ReadSection,
};
use parking_lot::RwLock;

use crate::curves::CurveMode;
use crate::motion_registry;
use crate::search::{self, UsageHit, LIVE_SECTION};

pub static EDIT_HANDLE: GlobalEditHandle = GlobalEditHandle::new();

static EFFECT_ITEMS_CACHE: LazyLock<RwLock<HashMap<String, Vec<EffectItemInfo>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTarget {
    pub effect_name: String,
    /// `set_object_item_value` の `":n"` に渡す番号＝**同じ名前のエフェクトの何番目か**（0 始まり）。
    pub effect_index: usize,
    /// エイリアス上の通し番号（`[Object.N]` の N）。
    /// ホストが `":n"` をこちらとして解釈する可能性が残るので、書けなかったときの再試行に使う。
    pub alias_index: usize,
    pub item_name: String,
    pub label: String,
    /// `list_focused_object_tracks` 取得時のトラック値（オーバーレイ再取得で API を叩かない）
    pub track_value: String,
    pub frame_start: usize,
    pub frame_end: usize,
}

/// 書き込み専用。本体 Undo に載るので読み取りには使わない（`with_read_section` を使う）。
pub fn with_edit_section<T, F>(job: &'static str, callback: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut EditSection) -> Result<T, String> + Send + 'static,
{
    if !EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません（AviUtl2 再起動後にお試しください）".into());
    }
    tracing::info!(job, "call_edit_section");
    EDIT_HANDLE
        .call_edit_section(callback)
        .map_err(edit_handle_error_message)
        .and_then(|result| result)
}

/// 読み取り専用 API（オーバーレイ取得など）。ウィンドウを隠さず、編集ロックのネストも避けられる。
pub fn with_read_section<T, F>(job: &'static str, callback: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&ReadSection) -> Result<T, String> + Send + 'static,
{
    if !EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません（AviUtl2 再起動後にお試しください）".into());
    }
    tracing::debug!(job, "call_read_section");
    EDIT_HANDLE
        .call_read_section(callback)
        .map_err(edit_handle_error_message)
        .and_then(|result| result)
}

fn edit_handle_error_message(err: EditHandleError) -> String {
    format!("編集 API エラー: {err:?}")
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "不明な panic".to_string()
    }
}

/// `call_edit_section` コールバック内の panic を FFI 越しに伝播させない。
pub fn catch_edit_panic<T, F>(job: &str, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + std::panic::UnwindSafe,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let msg = panic_payload_message(&*payload);
            tracing::error!(job, panic = %msg, "CurveEditor_H Edit API panic");
            Err(format!("内部エラー(panic): {msg}"))
        }
    }
}

/// 現在の編集モードからトラックバー移動スクリプトのセクション名を決定する。
pub fn motion_section_for(mode: CurveMode, preset_name: &str, slot: i32) -> (String, i32) {
    let query = search::build_search_query(preset_name, mode, slot);
    (query.section, if query.filter_slot { slot } else { 0 })
}

/// AviUtl2 に登録された移動方法の名前（トラック値の移動方法の欄の形式。本体が `.tra2` から登録する）。
pub fn motion_label_for_section(section: &str) -> String {
    motion_registry::motion_ini_key(section)
}

/// UI 表示用の適用先ラベル（適用ボタン押下前から見せる）。
pub fn format_apply_target(mode: CurveMode, preset_name: &str, slot: i32) -> String {
    let (section, apply_slot) = motion_section_for(mode, preset_name, slot);
    let key = motion_label_for_section(&section);
    if section == LIVE_SECTION && apply_slot > 0 {
        format!("適用先: Live #{apply_slot}（{key}）")
    } else {
        format!("適用先: {key}")
    }
}

/// トラック値から「値の並び」を取り出す。
///
/// 形式は `値1,…,値N,移動方法,パラメータ[|移動スクリプトの引数]`。
/// 中間点があると値が増え、**移動方法が付いていないトラックは値 1 つだけ**になる
/// （例: 音量調整 の `音量=100.00`）。数値として読める先頭の並びを値とみなす。
///
/// パラメータ側にカンマを含む式が入ることがある
/// （実物: `音量=95.00,移動無し,8|10^@min(音量/20-3,2)`）ので、
/// 後ろからではなく前から数えて値を確定させる。
fn parse_track_values(value: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for field in value.split(',') {
        let field = field.trim();
        if field.parse::<f64>().is_ok() {
            values.push(field.to_string());
        } else {
            break;
        }
    }
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// そのトラックがまだ移動方法を持っていない（固定値の）状態か。
pub fn track_has_no_motion(value: &str) -> bool {
    parse_track_values(value).is_some_and(|values| values.len() == 1)
}

/// 設定のビット 4（中間点無視）
const BIT_IGNORE_MIDPOINTS: u32 = 4;

/// `points` はオブジェクトの点（開始・中間点・終了）の数（`get_object_section_num` + 1）。分からなければ None。
///
/// **値の数は点の数に合わせる。** 本体は数を検査せずに保存し、合わないと動きが変わる（ルール `au2-rs-plugin`「設定項目の値の形式」）。
/// - 固定値（値 1 つ）は、点の数だけ同じ値を並べる（中間点が無ければ開始＝終了の 2 値。本体で移動方法を選んだときと同じ形）
/// - 値が 2 つで点が 3 つ以上なら、中間点無視（設定のビット 4）か再生範囲で 2 つに切り詰められたトラック。
///   中間点無視を立てたまま書く（外すと最初の区間だけ動いて後は止まる）
/// - 既に中間点無視が立っていれば、移動方法を切り替えても残す
pub fn build_motion_track_value(section: &str, slot: i32, existing: Option<&str>, points: Option<usize>) -> String {
    let motion = motion_label_for_section(section);
    let mut values = existing
        .and_then(parse_track_values)
        .unwrap_or_else(|| vec!["0".to_string(), "1".to_string()]);
    if values.len() == 1 {
        let n = points.unwrap_or(2).max(2);
        values = vec![values[0].clone(); n];
    }
    let had_ignore_bit = existing
        .and_then(split_motion)
        .and_then(|(_, param)| param.split('|').next()?.trim().parse::<u32>().ok())
        .is_some_and(|bits| bits & BIT_IGNORE_MIDPOINTS != 0);
    let ignore_midpoints = had_ignore_bit || points.is_some_and(|p| p > 2 && values.len() == 2);
    let values = values.join(",");
    let base_bits = if ignore_midpoints { BIT_IGNORE_MIDPOINTS } else { 0 };

    // 別の移動方法から引き継いだパラメータは捨てる（加減速のビットや設定値の意味が違う）。中間点無視だけは残す。
    // すでに Live のセクション（`カーブエディタ_H` / `区間ごとにスロットを進める` / `全体で1本`）の
    // トラックなら、そのセクションのまま設定欄（`加減速|スロット|時間制御`。.tra2 v1.10）の
    // スロットだけを差し替え、利用者が付けた加減速・時間制御を消さない。
    if section == LIVE_SECTION && slot > 0 {
        let kept = existing
            .and_then(split_motion)
            .filter(|(m, _)| is_live_motion(m))
            .and_then(|(m, param)| replace_live_slot(param, slot).map(|p| (m.to_string(), p)))
            .map(|(m, param)| (m, if ignore_midpoints { set_bits(&param, BIT_IGNORE_MIDPOINTS) } else { param }));
        match kept {
            Some((m, param)) => format!("{values},{m},{param}"),
            None => format!("{values},{motion},{base_bits}|{slot}"),
        }
    } else {
        format!("{values},{motion},{base_bits}")
    }
}

/// 設定欄の先頭（ビット）に `bits` を立てる。先頭が数値でなければそのまま返す
fn set_bits(param: &str, bits: u32) -> String {
    let (head, tail) = param.split_once('|').map_or((param, None), |(h, t)| (h, Some(t)));
    match head.trim().parse::<u32>() {
        Ok(b) => match tail {
            Some(t) => format!("{}|{t}", b | bits),
            None => format!("{}", b | bits),
        },
        Err(_) => param.to_string(),
    }
}

/// Live スロットを読む移動方法か（トラック値に入る `セクション@カーブエディタ_H` の形で比べる）。
fn is_live_motion(motion: &str) -> bool {
    std::iter::once(LIVE_SECTION)
        .chain(search::LIVE_VARIANT_SECTIONS.iter().copied())
        .any(|s| motion_label_for_section(s) == motion)
}

/// トラック値から (移動方法, パラメータ欄) を取り出す。移動方法が無ければ None。
/// パラメータ欄はカンマを含みうる（`2|9001|0,0.5,1,0`）ので、移動方法より後ろを丸ごと返す。
fn split_motion(value: &str) -> Option<(&str, &str)> {
    let mut rest = value;
    loop {
        let (field, tail) = rest.split_once(',')?;
        if field.trim().parse::<f64>().is_ok() {
            rest = tail;
        } else {
            return Some((field.trim(), tail));
        }
    }
}

/// カーブエディタ_H の設定欄のスロット（`|` の 2 番目の欄）を差し替える。
/// 形が想定と違えば None（呼び出し側で `0|スロット` に戻す）。
///
/// **設定値は必ずスロット 1 つだけにする。** .tra2 の `--param` は 1 つで、ホストは保存値の数が
/// `--param` の数と合わないと全部を既定値（スロット 1）に戻す（実機、2026-09-17 の検証 CRV-01 / 02）。
fn replace_live_slot(param: &str, slot: i32) -> Option<String> {
    let mut segs: Vec<String> = param.split('|').map(|s| s.to_string()).collect();
    if segs[0].parse::<u32>().is_err() {
        return None;
    }
    if segs.len() == 1 {
        return Some(format!("{}|{slot}", segs[0]));
    }
    if segs[1].split(',').any(|p| p.parse::<f64>().is_err()) {
        return None;
    }
    segs[1] = slot.to_string();
    Some(segs.join("|"))
}

/// エフェクト定義を一度だけキャッシュする（取得時の `get_effect_items` 呼び出しを避ける）。
/// 起動直後の `register()` では呼ばず、初回トラック取得時に遅延実行する。
pub fn warm_effect_items_cache() {
    if !EDIT_HANDLE.is_ready() {
        return;
    }
    if !EFFECT_ITEMS_CACHE.read().is_empty() {
        return;
    }
    let effects = EDIT_HANDLE.get_effects();
    for effect in effects {
        populate_effect_items_cache(&effect.name);
    }
    tracing::info!(
        cached_effects = EFFECT_ITEMS_CACHE.read().len(),
        "effect items cache warmed"
    );
}

fn populate_effect_items_cache(effect_name: &str) {
    let template = effect_name.split('@').next().unwrap_or(effect_name);
    for key in [effect_name, template] {
        if EFFECT_ITEMS_CACHE.read().contains_key(key) {
            continue;
        }
        if let Ok(items) = EDIT_HANDLE.get_effect_items(key) {
            let numeric: Vec<_> = items
                .into_iter()
                .filter(|item| {
                    matches!(
                        item.item_type,
                        EffectItemType::Number | EffectItemType::Integer
                    )
                })
                .collect();
            EFFECT_ITEMS_CACHE.write().insert(key.to_string(), numeric);
        }
    }
}

/// 取得時の項目フィルタ。キャッシュとヒューリスティックのみ（`get_effect_items` は呼ばない）。
fn is_numeric_track_item(effect_name: &str, item_name: &str, track_value: &str) -> bool {
    let template = effect_name.split('@').next().unwrap_or(effect_name);
    for key in [effect_name, template] {
        if let Some(items) = EFFECT_ITEMS_CACHE.read().get(key) {
            return items.iter().any(|item| item.name == item_name);
        }
    }
    is_heuristic_numeric_track(item_name, track_value)
}

fn is_heuristic_numeric_track(item_name: &str, track_value: &str) -> bool {
    let _ = item_name;
    // 移動が付いていれば確実に数値トラック（値の後ろに移動方法が続く）。
    if track_value.contains(',') && parse_track_values(track_value).is_some() {
        return true;
    }
    if track_value == "0" || track_value == "1" {
        return false;
    }
    track_value.parse::<f64>().is_ok()
}

/// フォーカスオブジェクトの軽い指紋（レイヤー・フレーム範囲・名前）。
/// 自動トラック更新の差分検出用。`call_read_section` のみ使う。
pub fn focused_object_fingerprint() -> Result<Option<String>, String> {
    with_read_section("focused_object_fingerprint", |read| {
        let Some(object) = read
            .get_focused_object()
            .map_err(|e| format!("{e:?}"))?
        else {
            return Ok(None);
        };
        let obj = read.object(object);
        let lf = obj.get_layer_frame().map_err(|e| format!("{e:?}"))?;
        let name = obj
            .get_name()
            .ok()
            .flatten()
            .unwrap_or_default();
        Ok(Some(format!(
            "L{}:{}-{}:{}",
            lf.layer, lf.start, lf.end, name
        )))
    })
}

/// フォーカスオブジェクトのトラックバー項目一覧を取得する。
///
/// 読み取りのみなので `call_read_section` を使う（`call_edit_section` は本体 Undo を汚染しうる）。
/// フィルタはキャッシュのみ（取得直後に `get_effect_items` を呼ばない）。
pub fn list_focused_object_tracks() -> Result<Vec<TrackTarget>, String> {
    warm_effect_items_cache();
    tracing::info!("list_focused_object_tracks: call_read_section");
    let (raw_entries, frame_start, frame_end) = with_read_section("list_focused_object_tracks", |read| {
        let Some(object) = read
            .get_focused_object()
            .map_err(|e| format!("{e:?}"))?
        else {
            return Err(
                "フォーカス中のオブジェクトがありません。タイムラインでオブジェクトを選択してください。"
                    .into(),
            );
        };

        let obj = read.object(object);
        let lf = obj.get_layer_frame().map_err(|e| format!("{e:?}"))?;
        let alias = obj.get_alias_parsed().map_err(|e| format!("{e:?}"))?;
        let entries = collect_track_entries_from_alias(&alias);
        Ok((entries, lf.start, lf.end))
    })?;
    tracing::info!(raw_entry_count = raw_entries.len(), "list_focused_object_tracks: read_section done");

    let raw_entry_count = raw_entries.len();
    let mut tracks = Vec::new();
    for entry in raw_entries {
        let RawTrackEntry {
            effect_name,
            effect_index,
            alias_index,
            item_name,
            track_value,
        } = entry;
        if !is_numeric_track_item(&effect_name, &item_name, &track_value) {
            continue;
        }
        tracks.push(TrackTarget {
            label: format!("{effect_name} / {item_name}"),
            effect_name,
            effect_index,
            alias_index,
            item_name,
            track_value,
            frame_start,
            frame_end,
        });
    }
    tracing::info!(track_count = tracks.len(), "list_focused_object_tracks: filter done");

    if tracks.is_empty() {
        tracing::warn!(
            raw_entry_count,
            "list_focused_object_tracks: no numeric tracks after filter"
        );
        return Err("フォーカスオブジェクトにトラックバー項目が見つかりません".into());
    }
    tracks.sort_by(|a, b| a.label.cmp(&b.label));
    tracing::info!(track_count = tracks.len(), "list_focused_object_tracks: success");
    Ok(tracks)
}

#[derive(Debug, Clone)]
struct RawTrackEntry {
    effect_name: String,
    effect_index: usize,
    alias_index: usize,
    item_name: String,
    track_value: String,
}

fn collect_track_entries_from_alias(alias: &aviutl2::alias::Table) -> Vec<RawTrackEntry> {
    let mut entries = Vec::new();
    let mut seen_names: HashMap<String, usize> = HashMap::new();
    walk_alias_tables("", alias, &mut entries, &mut seen_names);
    entries
}

fn walk_alias_tables(
    prefix: &str,
    table: &aviutl2::alias::Table,
    out: &mut Vec<RawTrackEntry>,
    seen_names: &mut HashMap<String, usize>,
) {
    if table.get_value("effect.name").is_some() {
        collect_effect_entries(prefix, table, out, seen_names);
        return;
    }
    for (key, sub) in table.subtables() {
        let child_prefix = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };
        walk_alias_tables(&child_prefix, sub, out, seen_names);
    }
}

/// `[Object.2]` の 2。エイリアス上の並び順。
fn effect_index_from_alias_key(key: &str) -> usize {
    key.rsplit('.')
        .next()
        .and_then(|segment| segment.parse().ok())
        .unwrap_or(0)
}

fn collect_effect_entries(
    prefix: &str,
    effect_table: &aviutl2::alias::Table,
    out: &mut Vec<RawTrackEntry>,
    seen_names: &mut HashMap<String, usize>,
) {
    let Some(effect_name) = effect_table.get_value("effect.name") else {
        return;
    };
    // `set_object_item_value` の ":n" は **同じ名前のエフェクトの何番目か**（0 始まり）で、
    // エイリアス上の通し番号ではない（`plugin2.h`:「同じエフェクトが複数ある場合は":n"の
    // サフィックスでインデックス指定出来ます」／ClaudeBridge の list_object_effects も同じ数え方）。
    // ここを [Object.N] の N にしていたため、先頭以外に置いたエフェクトへ書けなかった。
    let counter = seen_names.entry(effect_name.clone()).or_insert(0);
    let effect_index = *counter;
    *counter += 1;
    let alias_index = effect_index_from_alias_key(prefix);
    for (item_name, track_value) in effect_table.values() {
        if item_name == "effect.name" {
            continue;
        }
        out.push(RawTrackEntry {
            effect_name: effect_name.clone(),
            effect_index,
            alias_index,
            item_name: item_name.clone(),
            track_value: track_value.clone(),
        });
    }
}

/// 成功したら実際に書き込んだトラック値を返す（呼び出し側のキャッシュ更新用）。
pub fn apply_to_track(
    target: &TrackTarget,
    section: &str,
    slot: i32,
) -> Result<String, String> {
    if section != LIVE_SECTION
        && !search::BUILTIN_SECTIONS.iter().any(|s| *s == section)
    {
        return Err(format!(
            "適用できないセクションです: {section}（カーブエディタ_Hの組み込みセクションまたは Live スロットのみ対応）"
        ));
    }

    // 登録されていない名前を書くと本体が `not found movement` で編集を打ち切るので、書く前に .tra2 で確かめる
    motion_registry::ensure_section_registered(section)?;

    // 読み取りはキャッシュ済み track_value を使い、edit section 内は set のみ（Undo 汚染を最小化）。
    if parse_track_values(&target.track_value).is_none() {
        return Err(format!(
            "「{}」は数値トラックではないためカーブエディタ_Hを適用できません（現在値: {}）",
            target.label, target.track_value
        ));
    }
    let section = section.to_string();
    let effect_name = target.effect_name.clone();
    let effect_index = target.effect_index;
    let alias_index = target.alias_index;
    let item_name = target.item_name.clone();
    let old_value = target.track_value.clone();
    with_edit_section("apply_to_track", move |edit| {
        let Some(obj_handle) = edit
            .get_focused_object()
            .map_err(|e| format!("{e:?}"))?
        else {
            return Err("フォーカス中のオブジェクトがありません".into());
        };
        // 値の数を点の数に合わせるため、書く直前に中間点の数を読む
        let points = edit.get_object_section_num(obj_handle).ok().map(|n| n + 1);
        let new_value = build_motion_track_value(&section, slot, Some(&old_value), points);
        let object = edit.object(obj_handle);

        // `":n"` の解釈がホスト側で確定していないので、書く前に読んで狙いを定める。
        // 第一候補は SDK どおりの「同名エフェクトの何番目か」、
        // 第二候補はエイリアス上の通し番号（従来 CurveEditor_H が送っていた番号）。
        let mut candidates = vec![effect_index];
        if alias_index != effect_index {
            candidates.push(alias_index);
        }

        // 取得時に見えていた値と一致する番号を選ぶ。
        let mut chosen = None;
        for &idx in &candidates {
            match object.get_effect_item(&effect_name, idx, &item_name) {
                Ok(current) if current == old_value => {
                    chosen = Some(idx);
                    break;
                }
                Ok(current) => tracing::debug!(
                    effect = %effect_name, idx, item = %item_name,
                    current = %current, expected = %old_value,
                    "apply_to_track: index candidate points elsewhere"
                ),
                Err(e) => tracing::debug!(
                    effect = %effect_name, idx, item = %item_name, error = ?e,
                    "apply_to_track: index candidate not readable"
                ),
            }
        }

        let Some(idx) = chosen else {
            tracing::warn!(
                effect = %effect_name, effect_index, alias_index, item = %item_name,
                expected = %old_value,
                "apply_to_track: no effect index addresses the fetched track"
            );
            return Err(format!(
                "対象トラックを特定できませんでした（{effect_name} / {item_name}）。「トラック取得」を押し直してからもう一度お試しください"
            ));
        };

        object
            .set_effect_item(&effect_name, idx, &item_name, &new_value)
            .map_err(|e| {
                // ホストのログファイルに残るのは WARN 以上。原因が追えるよう中身を出す。
                tracing::warn!(
                    effect = %effect_name, idx, item = %item_name,
                    value = %new_value, error = ?e,
                    "apply_to_track: set_effect_item failed"
                );
                format!("トラックに書き込めませんでした（{effect_name}:{idx} / {item_name}）: {e:?}")
            })?;

        // 書けたと言われても値が入っていないことがあるので読み返す。
        // 「エラーは出ないのに移動方法が変わらない」を成功として報告しない。
        match object.get_effect_item(&effect_name, idx, &item_name) {
            Ok(written) if written == new_value => Ok(new_value),
            Ok(written) => {
                tracing::warn!(
                    effect = %effect_name, idx, item = %item_name,
                    wrote = %new_value, read_back = %written,
                    "apply_to_track: value did not stick"
                );
                Err(format!(
                    "書き込みが反映されませんでした（{effect_name}:{idx} / {item_name}）。現在値: {written}"
                ))
            }
            Err(e) => {
                // 読み返せないだけかもしれないので、成功扱いにしつつログに残す。
                tracing::warn!(
                    effect = %effect_name, idx, item = %item_name, error = ?e,
                    "apply_to_track: read back failed"
                );
                Ok(new_value)
            }
        }
    })
}

/// 今のシーンの使用箇所を、本体の今の状態（オブジェクトエイリアス）から拾う（v0.9.5）。
///
/// 保存済みの `.aup2` だけを読んでいた v0.9.4 までは、保存前の変更（「このトラックに適用」の直後など）を
/// 数えなかった（`issues/20260924_usage_search_reads_saved_file_only.md`）。読み取りだけなので
/// `call_read_section` を使う（本体 Undo に触れない。検索ボタンを押したときだけ呼ぶ）。
/// 戻り値は `(シーン ID, 見つかったもの)`。本体の API はほかのシーンに届かない。
pub fn search_current_scene(query: &search::SearchQuery) -> Result<(i32, Vec<UsageHit>), String> {
    if !EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません（AviUtl2 再起動後にお試しください）".into());
    }
    let info = EDIT_HANDLE.get_edit_info();
    let scene = info.scene_id;
    // 空のシーンで負の値が来ても回らないようにする（usize への変換で巨大になる）
    let layer_max = if info.layer_max > 100_000 { None } else { Some(info.layer_max) };
    let query = query.clone();
    let hits = with_read_section("search_current_scene", move |read| {
        let mut hits = Vec::new();
        let Some(layer_max) = layer_max else {
            return Ok(hits);
        };
        for layer in 0..=layer_max {
            for (lf, object) in read.objects_in_layer(layer) {
                let alias = read.get_object_alias(object).map_err(|e| format!("{e:?}"))?;
                for (effect_name, track_key, section, slot) in search::search_alias_text(&alias, &query) {
                    hits.push(UsageHit {
                        scene,
                        layer: layer as i32,
                        frame_b: lf.start as i32,
                        frame_e: lf.end as i32,
                        effect_name,
                        track_key,
                        section,
                        slot,
                    });
                }
            }
        }
        Ok(hits)
    })?;
    Ok((scene, hits))
}

pub fn focus_usage_hit(hit: &UsageHit) -> Result<(), String> {
    let hit = hit.clone();
    with_edit_section("focus_usage_hit", move |edit| {
        let handle = find_object_for_hit(edit, &hit)?;
        edit.object(handle)
            .set_focus_object()
            .map_err(|e| format!("{e:?}"))?;
        Ok(())
    })
}

fn find_object_for_hit(edit: &mut EditSection, hit: &UsageHit) -> Result<ObjectHandle, String> {
    if edit.info.scene_id != hit.scene {
        return Err(format!(
            "シーンが一致しません (現在={}, 検索={})",
            edit.info.scene_id, hit.scene
        ));
    }
    let layer_idx = usize::try_from(hit.layer).map_err(|_| "レイヤー番号が不正です".to_string())?;
    let frame_b = usize::try_from(hit.frame_b).map_err(|_| "フレーム番号が不正です".to_string())?;
    let frame_e = usize::try_from(hit.frame_e).map_err(|_| "フレーム番号が不正です".to_string())?;
    for (position, object) in edit.layer(layer_idx).objects() {
        if position.start == frame_b && position.end == frame_e {
            return Ok(object);
        }
    }
    Err(format!(
        "オブジェクトが見つかりません: S{} L{} F{}-{}",
        hit.scene, hit.layer, hit.frame_b, hit.frame_e
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 実物と同じ形のオブジェクトエイリアス。
    /// - 先頭以外にスクリプト効果を置く（マルチセクション名 `@` 入り）
    /// - 同名エフェクトを 2 つ並べる
    const ALIAS: &str = "[Object]
frame=0,59
name=test
[Object.0]
effect.name=図形
サイズ=100
[Object.1]
effect.name=標準描画
X=0.00
Y=0.00,100.00,直線移動,0
[Object.2]
effect.name=色調補正@Basic_S
輝度=100.00
その他.hide=1
[Object.3]
effect.name=色調補正@Basic_S
輝度=50.00
";

    fn entries() -> Vec<RawTrackEntry> {
        let table = aviutl2::alias::Table::from_str(ALIAS).expect("alias parses");
        collect_track_entries_from_alias(&table)
    }

    fn find<'a>(v: &'a [RawTrackEntry], effect: &str, item: &str) -> Vec<&'a RawTrackEntry> {
        v.iter()
            .filter(|e| e.effect_name == effect && e.item_name == item)
            .collect()
    }

    /// `":n"` は同名エフェクトの通し番号。エイリアスの `[Object.N]` の N ではない。
    #[test]
    fn effect_index_counts_same_named_effects_only() {
        let v = entries();

        // 先頭以外に置いた効果でも 0（その名前では 1 個目）。
        let draw = find(&v, "標準描画", "X");
        assert_eq!(draw.len(), 1);
        assert_eq!(draw[0].effect_index, 0, "標準描画 は [Object.1] だが同名は 1 個目");

        // マルチセクションスクリプト（`@` 入り）も同じ扱い。
        let hosei = find(&v, "色調補正@Basic_S", "輝度");
        assert_eq!(hosei.len(), 2);
        assert_eq!(hosei[0].effect_index, 0, "1 個目の 色調補正@Basic_S");
        assert_eq!(hosei[1].effect_index, 1, "2 個目の 色調補正@Basic_S");
    }

    /// `effect.name` 自体は項目として拾わない。`.` を含むキー（グループの開閉状態）は
    /// サブテーブルにならず値のまま残る。
    #[test]
    fn effect_name_is_not_an_item_and_dotted_keys_stay_flat() {
        let v = entries();
        assert!(v.iter().all(|e| e.item_name != "effect.name"));
        assert_eq!(find(&v, "色調補正@Basic_S", "その他.hide").len(), 1);
    }

    /// トラック値の分解（適用可否の入口）。
    #[test]
    fn parse_track_values_handles_every_form() {
        assert_eq!(
            parse_track_values("0.00,100.00,直線移動,0"),
            Some(vec!["0.00".to_string(), "100.00".to_string()])
        );
        assert_eq!(
            parse_track_values("0.00,100.00,カーブエディタ_H@カーブエディタ_H,0|3"),
            Some(vec!["0.00".to_string(), "100.00".to_string()])
        );
        // 中間点つき（値が 3 つ）
        assert_eq!(
            parse_track_values("0.00,50.00,100.00,直線移動,0"),
            Some(vec!["0.00".to_string(), "50.00".to_string(), "100.00".to_string()])
        );
        // 移動方法なしの固定値（音量調整 の 音量 がこれ）
        assert_eq!(parse_track_values("100.00"), Some(vec!["100.00".to_string()]));
        assert!(track_has_no_motion("100.00"));
        assert!(!track_has_no_motion("0.00,100.00,直線移動,0"));
        // パラメータにカンマを含む式が入る実例
        assert_eq!(
            parse_track_values("95.00,移動無し,8|10^@min(音量/20-3,2)"),
            Some(vec!["95.00".to_string()])
        );
        // 数値で始まらないものは対象外
        assert!(parse_track_values("通常").is_none());
    }

    /// Live スロットの書き込み値。既存の値の並びを引き継ぐ。
    #[test]
    fn build_motion_track_value_keeps_values() {
        let v = build_motion_track_value(LIVE_SECTION, 3, Some("-200.00,0.00,直線移動,0"), None);
        assert!(v.starts_with("-200.00,0.00,"), "endpoints kept: {v}");
        assert!(v.ends_with(",0|3"), "param and slot: {v}");

        // 中間点は個数を保つ（3 値のまま）
        let v = build_motion_track_value(LIVE_SECTION, 3, Some("0.00,50.00,100.00,直線移動,0"), None);
        assert!(v.starts_with("0.00,50.00,100.00,"), "keyframes kept: {v}");

        // 固定値は開始＝終了の 2 値へ広げる（音量調整 の 音量 が通るようになる）
        let v = build_motion_track_value(LIVE_SECTION, 2, Some("100.00"), None);
        assert_eq!(v, "100.00,100.00,カーブエディタ_H@カーブエディタ_H,0|2");

        // 他の移動方法のパラメータは引き継がない（加減速のビットや設定値の意味が違う）
        let v = build_motion_track_value(LIVE_SECTION, 1, Some("95.00,移動無し,8|10^@min(音量/20-3,2)"), None);
        assert_eq!(v, "95.00,95.00,カーブエディタ_H@カーブエディタ_H,0|1");
        let v = build_motion_track_value(LIVE_SECTION, 7, Some("-200.00,0.00,指数関数@Basic_S,2|6"), None);
        assert_eq!(v, "-200.00,0.00,カーブエディタ_H@カーブエディタ_H,0|7");
    }

    /// すでに Live のセクションのトラックなら、セクション・加減速・時間制御を残してスロットだけ変える（.tra2 v1.10）。
    #[test]
    fn build_motion_track_value_keeps_live_settings() {
        let live = "カーブエディタ_H@カーブエディタ_H";
        // v1.0 の形
        let v = build_motion_track_value(LIVE_SECTION, 7, Some(&format!("0.00,100.00,{live},0|5")), None);
        assert_eq!(v, format!("0.00,100.00,{live},0|7"));
        // 実制作にある 4|スロット（ビットの意味は未確認。そのまま残す）
        let v = build_motion_track_value(LIVE_SECTION, 7, Some(&format!("0.000,1.000,{live},4|1")), None);
        assert_eq!(v, format!("0.000,1.000,{live},4|7"));
        // 加減速・時間制御つき、中間点あり
        let v = build_motion_track_value(
            LIVE_SECTION,
            12,
            Some(&format!("0.00,50.00,10.00,{live},3|5|0,0.511224,1,0")),
            None,
        );
        assert_eq!(v, format!("0.00,50.00,10.00,{live},3|12|0,0.511224,1,0"));
        // Live の別セクションはセクションのまま
        let v = build_motion_track_value(
            LIVE_SECTION,
            9,
            Some("0.00,50.00,10.00,全体で1本@カーブエディタ_H,2|4|0,0.511224,1,0"),
            None,
        );
        assert_eq!(v, "0.00,50.00,10.00,全体で1本@カーブエディタ_H,2|9|0,0.511224,1,0");
        let v = build_motion_track_value(LIVE_SECTION, 3, Some("0.00,1.00,区間ごとにスロットを進める@カーブエディタ_H,0|1"), None);
        assert_eq!(v, "0.00,1.00,区間ごとにスロットを進める@カーブエディタ_H,0|3");
        // 設定値はスロット 1 つだけに揃える（数が合わないとホストが全部を既定値に戻す）
        let v = build_motion_track_value(LIVE_SECTION, 7, Some(&format!("0.00,1.00,{live},0|5,1")), None);
        assert_eq!(v, format!("0.00,1.00,{live},0|7"));
        // 組み込み（スロットを使わない）セクションからは Live の 0|スロット に切り替える
        let v = build_motion_track_value(LIVE_SECTION, 7, Some("0.00,1.00,バウンス@カーブエディタ_H,0"), None);
        assert_eq!(v, format!("0.00,1.00,{live},0|7"));
        // 形が崩れていれば 0|スロット に戻す
        let v = build_motion_track_value(LIVE_SECTION, 7, Some(&format!("0.00,1.00,{live},x|5")), None);
        assert_eq!(v, format!("0.00,1.00,{live},0|7"));
        let v = build_motion_track_value(LIVE_SECTION, 7, Some(&format!("0.00,1.00,{live},0|a")), None);
        assert_eq!(v, format!("0.00,1.00,{live},0|7"));
    }

    /// 値の数を点の数に合わせる（公開前チェックリストの点検、`issues/20261010_publish_checklist_audit.md`）
    #[test]
    fn build_motion_track_value_matches_points() {
        let live = "カーブエディタ_H@カーブエディタ_H";
        // 固定値は点の数だけ並べる（中間点 1 つ = 点 3 つ）
        let v = build_motion_track_value(LIVE_SECTION, 2, Some("100.00"), Some(3));
        assert_eq!(v, format!("100.00,100.00,100.00,{live},0|2"));
        // 中間点無視で 2 つに切り詰められたトラックは、ビット 4 を残したまま書く
        let v = build_motion_track_value(LIVE_SECTION, 5, Some("0.00,100.00,直線移動,4"), Some(4));
        assert_eq!(v, format!("0.00,100.00,{live},4|5"));
        // 再生範囲（ビットは無いが値 2 つ）で点が 3 つ以上なら、中間点無視を立てる
        let v = build_motion_track_value(LIVE_SECTION, 5, Some("0.00,100.00,再生範囲,0"), Some(3));
        assert_eq!(v, format!("0.00,100.00,{live},4|5"));
        // Live のまま中間点無視が付いていれば残す（加減速のビットも残す）
        let v = build_motion_track_value(LIVE_SECTION, 8, Some(&format!("0.00,100.00,{live},7|5")), Some(4));
        assert_eq!(v, format!("0.00,100.00,{live},7|8"));
        // Live で値 2 つ・点 3 つ・ビット無しなら、ビット 4 を足す
        let v = build_motion_track_value(LIVE_SECTION, 8, Some(&format!("0.00,100.00,{live},1|5")), Some(3));
        assert_eq!(v, format!("0.00,100.00,{live},5|8"));
        // 点の数と合っていれば何も足さない
        let v = build_motion_track_value(LIVE_SECTION, 3, Some("0.00,50.00,100.00,直線移動,0"), Some(3));
        assert_eq!(v, format!("0.00,50.00,100.00,{live},0|3"));
        // 組み込みのセクション（スロット無し）でも同じ
        let v = build_motion_track_value("バウンス", 0, Some("0.00,100.00,直線移動,4"), Some(5));
        assert!(v.ends_with(",4"), "{v}");
        assert_eq!(set_bits("2|9|0,1", 4), "6|9|0,1");
        assert_eq!(set_bits("x|9", 4), "x|9");
    }

    #[test]
    fn split_motion_takes_everything_after_motion() {
        assert_eq!(
            split_motion("0.00,100.00,カーブエディタ_H@カーブエディタ_H,2|9001|0,0.5,1,0"),
            Some(("カーブエディタ_H@カーブエディタ_H", "2|9001|0,0.5,1,0"))
        );
        assert_eq!(split_motion("100.00"), None);
    }
}
