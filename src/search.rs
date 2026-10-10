//! `.aup2` 内の CurveEditor_H 使用箇所検索

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::curves::CurveMode;

/// トラックバー移動方法の表示名（`--label:` / `aviutl2.ini` の Movement 名）。
pub const MOTION_LABEL: &str = "カーブエディタ_H";

/// Live スロット連携セクション（`@カーブエディタ_H.tra2` の `@カーブエディタ_H`）。
pub const LIVE_SECTION: &str = "カーブエディタ_H";

/// ScriptCurve 時代の移動名（検索の後方互換用）。
pub const LEGACY_MOTION_LABEL: &str = "スクリプトカーブ";

/// `@カーブエディタ_H.tra2` に定義された組み込みセクション名。
/// .tra2 と一致していることを `AI/scripts/CurveEditor_H/verify_tra2.py` が確かめる
/// （v0.9.1 / .tra2 v1.10 で「加減速」を外した）。
pub const BUILTIN_SECTIONS: &[&str] = &[
    "カーブエディタ_H",
    "バウンス",
    "ループバウンス",
    "弾性",
    "バック",
    "ベジェ",
    "サイン",
    "指数アウト",
    "ステップ",
    "スプリング",
    "パーリン揺らぎ",
];

/// Live スロットを読むほかのセクション（.tra2 v1.10。本文は `@カーブエディタ_H` と同じで、区間の扱いだけが違う）。
/// 設定値（スロット）の数を v1.0 のトラックと合わせるため、区間の扱いは設定値ではなくセクションで分けている。
/// `BUILTIN_SECTIONS` には入れない（あちらは「スロットを使わない組み込み」の一覧）。
pub const LIVE_VARIANT_SECTIONS: &[&str] = &["区間ごとにスロットを進める", "全体で1本"];

/// Live スロットを読むセクションか（`@カーブエディタ_H` と `LIVE_VARIANT_SECTIONS`）。
pub fn is_live_section(section: &str) -> bool {
    section == LIVE_SECTION || LIVE_VARIANT_SECTIONS.contains(&section)
}

const SEARCH_TAGS: &[&str] = &["@カーブエディタ_H", "@スクリプトカーブ"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub section: String,
    pub slot: i32,
    pub filter_slot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageHit {
    pub scene: i32,
    pub layer: i32,
    pub frame_b: i32,
    pub frame_e: i32,
    pub effect_name: String,
    pub track_key: String,
    pub section: String,
    pub slot: i32,
}

impl UsageHit {
    pub fn format_line(&self) -> String {
        let mut s = format!(
            "F{}-{} S{} L{}",
            self.frame_b, self.frame_e, self.scene, self.layer
        );
        if !self.effect_name.is_empty() {
            s.push(' ');
            s.push_str(&self.effect_name);
        }
        s.push_str(" / ");
        s.push_str(&self.track_key);
        if self.section == LIVE_SECTION && self.slot > 0 {
            s.push_str(&format!(" (slot{})", self.slot));
        } else if is_live_section(&self.section) && self.slot > 0 {
            s.push_str(&format!(" (slot{} {})", self.slot, self.section));
        } else {
            s.push_str(&format!(" ({})", self.section));
        }
        s
    }
}

/// 現在の編集状態から検索クエリを構築する。
pub fn build_search_query(preset_name: &str, _mode: CurveMode, slot: i32) -> SearchQuery {
    let name = preset_name.trim();
    if !name.is_empty() {
        for sec in BUILTIN_SECTIONS {
            if *sec == LIVE_SECTION {
                continue;
            }
            if name == *sec {
                return SearchQuery {
                    section: sec.to_string(),
                    slot,
                    filter_slot: false,
                };
            }
        }
    }

    SearchQuery {
        section: LIVE_SECTION.to_string(),
        slot,
        filter_slot: true,
    }
}

pub fn search_aup2_file(path: &Path, query: &SearchQuery) -> Vec<UsageHit> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);

    let mut hits = Vec::new();
    let mut scene = 0;
    let mut layer = 0;
    let mut frame_b = 0;
    let mut frame_e = 0;
    let mut effect_name = String::new();
    let mut in_object = false;

    for line in reader.lines().map_while(Result::ok) {
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') {
            let Some(end) = line.find(']') else { continue };
            if end < 2 {
                continue;
            }
            let sec = &line[1..end];
            if sec.starts_with("scene.") || sec.starts_with("layer.") {
                in_object = false;
                continue;
            }
            if is_object_section_name(sec) {
                in_object = true;
                scene = 0;
                layer = 0;
                frame_b = 0;
                frame_e = 0;
                effect_name.clear();
                continue;
            }
            if in_object && sec.contains('.') {
                effect_name.clear();
            }
            continue;
        }

        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        if !in_object {
            continue;
        }

        match key {
            "scene" => scene = val.parse().unwrap_or(0),
            "layer" => layer = val.parse().unwrap_or(0),
            "frame" => {
                // 中間点があると `開始,中間点…,終了` と 3 値以上になる。先頭と末尾を取る
                let mut it = val.split(',');
                frame_b = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                frame_e = it.last().and_then(|v| v.parse().ok()).unwrap_or(frame_b);
            }
            "effect.name" => effect_name = val.to_string(),
            _ => {
                if let Some((track_key, section, slot)) = match_track_line(line, query) {
                    hits.push(UsageHit {
                        scene,
                        layer,
                        frame_b,
                        frame_e,
                        effect_name: effect_name.clone(),
                        track_key,
                        section,
                        slot,
                    });
                }
            }
        }
    }

    dedupe_hits(hits)
}

/// 1 行がクエリに合うトラックなら `(項目名, セクション, スロット)` を返す。
fn match_track_line(line: &str, query: &SearchQuery) -> Option<(String, String, i32)> {
    if !SEARCH_TAGS.iter().any(|tag| line.contains(tag)) {
        return None;
    }
    let (track_key, section, slot) = parse_motion_trackline(line)?;
    hit_matches_query(query, &section, slot).then_some((track_key, section, slot))
}

/// オブジェクトエイリアス 1 つ分（`[Object]` / `[Object.N]` の INI）から、クエリに合うトラックを拾う。
///
/// 戻り値は `(効果名, 項目名, セクション, スロット)`。シーン・レイヤー・フレームはエイリアスに無いので、
/// 呼び出し側（本体の今の状態を読む `edit_ops::search_current_scene`）が埋める。
pub fn search_alias_text(alias: &str, query: &SearchQuery) -> Vec<(String, String, String, i32)> {
    let mut out = Vec::new();
    let mut effect_name = String::new();
    for line in alias.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with('[') {
            effect_name.clear();
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        if key == "effect.name" {
            effect_name = val.to_string();
            continue;
        }
        if let Some((track_key, section, slot)) = match_track_line(line, query) {
            out.push((effect_name.clone(), track_key, section, slot));
        }
    }
    out
}

/// 今のシーンは本体の状態から読んだ結果（`live`）を使い、ほかのシーンだけ保存済みファイルの結果（`saved`）を足す。
/// 本体の API は今のシーンにしか届かないので、ほかのシーンは保存済みのファイルに頼るしかない。
pub fn merge_live_and_saved(current_scene: i32, live: Vec<UsageHit>, saved: Vec<UsageHit>) -> Vec<UsageHit> {
    let mut hits = live;
    hits.extend(saved.into_iter().filter(|h| h.scene != current_scene));
    hits.sort_by(|a, b| {
        (a.scene != current_scene, a.scene, a.layer, a.frame_b)
            .cmp(&(b.scene != current_scene, b.scene, b.layer, b.frame_b))
    });
    dedupe_hits(hits)
}

fn is_object_section_name(name: &str) -> bool {
    !name.contains('.')
}

pub fn parse_motion_trackline(line: &str) -> Option<(String, String, i32)> {
    let tag = SEARCH_TAGS
        .iter()
        .find_map(|t| line.find(t).map(|pos| (*t, pos)))?;
    let eq = line.find('=')?;
    let track_key = line[..eq].to_string();

    let sec_start = line[..tag.1].rfind(',')?;
    let section = line[sec_start + 1..tag.1].to_string();

    // 設定欄は `加減速|スロット,区間|時間制御`（.tra2 v1.10）。スロットは 2 番目の欄の先頭。
    // v1.0 の `0|スロット` も同じ読み方になる。末尾の `|` の後ろを読むと、区間や時間制御を
    // 付けたトラック（`0|8,1` / `0|8,0|0,0.5,1,0`）でスロットが 0 になる
    let param = line[tag.1..].split_once(',').map(|(_, p)| p).unwrap_or("");
    let slot = param
        .split('|')
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    Some((track_key, section, slot))
}

pub fn normalize_motion_section(section: &str) -> &str {
    // Live の別セクション（区間の扱いだけが違う）も、スロットの使用箇所としては同じに数える
    if is_live_section(section) || section == LEGACY_MOTION_LABEL {
        return LIVE_SECTION;
    }
    for prefix in [
        format!("{MOTION_LABEL}["),
        format!("{LEGACY_MOTION_LABEL}["),
    ] {
        if let Some(inner) = section.strip_prefix(&prefix).and_then(|s| s.strip_suffix(']')) {
            return inner;
        }
    }
    if let Some(inner) = section.strip_prefix(&format!("{MOTION_LABEL}-")) {
        return inner;
    }
    if let Some(inner) = section.strip_prefix(&format!("{LEGACY_MOTION_LABEL}-")) {
        return inner;
    }
    section
}

fn hit_matches_query(query: &SearchQuery, section: &str, slot: i32) -> bool {
    if normalize_motion_section(section) != query.section {
        return false;
    }
    if query.filter_slot && slot != query.slot {
        return false;
    }
    true
}

fn dedupe_hits(hits: Vec<UsageHit>) -> Vec<UsageHit> {
    let mut out: Vec<UsageHit> = Vec::new();
    'outer: for h in hits {
        for e in &out {
            if e.scene == h.scene
                && e.layer == h.layer
                && e.frame_b == h.frame_b
                && e.frame_e == h.frame_e
                && e.effect_name == h.effect_name
                && e.track_key == h.track_key
                && e.section == h.section
                && e.slot == h.slot
            {
                continue 'outer;
            }
        }
        out.push(h);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_trackline_ce2() {
        let line = "しぼり=0.00,0.85,カーブエディタ_H@カーブエディタ_H,0|8";
        let (key, sec, slot) = parse_motion_trackline(line).unwrap();
        assert_eq!(key, "しぼり");
        assert_eq!(sec, "カーブエディタ_H");
        assert_eq!(slot, 8);
    }

    /// .tra2 v1.10 の設定欄（加減速・時間制御つき）と、Live の別セクションでもスロットを読める。
    #[test]
    fn parse_trackline_ce2_settings() {
        for (line, want_sec, want) in [
            ("X=0.00,1.00,カーブエディタ_H@カーブエディタ_H,3|12|0,0.511224,1,0", "カーブエディタ_H", 12),
            ("X=0.00,50.00,1.00,カーブエディタ_H@カーブエディタ_H,4|3", "カーブエディタ_H", 3),
            ("X=0.00,1.00,カーブエディタ_H@カーブエディタ_H,0", "カーブエディタ_H", 0),
            ("X=0.00,50.00,1.00,全体で1本@カーブエディタ_H,2|8|0,0.5,1,0", "全体で1本", 8),
            ("X=0.00,50.00,1.00,区間ごとにスロットを進める@カーブエディタ_H,0|5", "区間ごとにスロットを進める", 5),
        ] {
            let (_, sec, slot) = parse_motion_trackline(line).unwrap();
            assert_eq!(sec, want_sec, "{line}");
            assert_eq!(slot, want, "{line}");
        }
        // 使用箇所検索では Live の別セクションも同じスロットとして数える
        let q = SearchQuery { section: LIVE_SECTION.to_string(), slot: 8, filter_slot: true };
        assert!(hit_matches_query(&q, "全体で1本", 8));
        assert!(!hit_matches_query(&q, "全体で1本", 9));
        assert!(!hit_matches_query(&q, "バウンス", 8));
    }

    #[test]
    fn parse_trackline_legacy() {
        let line = "しぼり=0.00,0.85,スクリプトカーブ@スクリプトカーブ,0|8";
        let (key, sec, slot) = parse_motion_trackline(line).unwrap();
        assert_eq!(key, "しぼり");
        assert_eq!(sec, "スクリプトカーブ");
        assert_eq!(slot, 8);
    }

    #[test]
    fn build_query_live_slot() {
        let q = build_search_query("", CurveMode::Composite, 5);
        assert_eq!(q.section, LIVE_SECTION);
        assert!(q.filter_slot);
        assert_eq!(q.slot, 5);
    }

    #[test]
    fn normalize_motion_section_bracket() {
        assert_eq!(
            normalize_motion_section("カーブエディタ_H[バウンス]"),
            "バウンス"
        );
        assert_eq!(
            normalize_motion_section("スクリプトカーブ[バウンス]"),
            "バウンス"
        );
    }

    #[test]
    fn build_query_builtin_preset() {
        let q = build_search_query("バウンス", CurveMode::Script, 1);
        assert_eq!(q.section, "バウンス");
        assert!(!q.filter_slot);
    }

    /// 使用箇所検索を、テスト内で組み立てた `.aup2` に対して走らせる。
    /// 以前は実環境の restart.aup2（ホストが最後に開いていた状態）に依存し、
    /// その時の編集内容次第で落ちていた（issues/20260910_search_test_depends_on_local_restart_aup2.md）。
    #[test]
    fn search_aup2_file_hits_and_misses() {
        let text = [
            "[project]", "version=2011000",
            "[scene.0]", "scene=0", "name=Root",
            "[0]", "layer=0", "frame=0,59",
            "[0.0]", "effect.name=図形",
            "[0.1]", "effect.name=標準描画",
            "X=0.00,100.00,カーブエディタ_H@カーブエディタ_H,0|8",
            "Y=0.00,100.00,カーブエディタ_H@カーブエディタ_H,0|9",
            "[1]", "layer=3", "scene=1", "frame=10,15,20",
            "[1.0]", "effect.name=グレイン@Filters_H",
            "強度=0.0,50.0,100.0,全体で1本@カーブエディタ_H,2|8|0,0.5,1,0",
            "[2]", "layer=1", "frame=30,40",
            "[2.0]", "effect.name=図形",
            "サイズ=100,200,直線移動,0",
        ]
        .join("\r\n");
        let path = std::env::temp_dir().join(format!("ce2_search_test_{}.aup2", std::process::id()));
        std::fs::write(&path, &text).unwrap();

        let q = SearchQuery { section: LIVE_SECTION.to_string(), slot: 8, filter_slot: true };
        let q7 = SearchQuery { section: LIVE_SECTION.to_string(), slot: 7, filter_slot: true };
        let hits = search_aup2_file(&path, &q);
        let none = search_aup2_file(&path, &q7);
        let _ = std::fs::remove_file(&path);

        // スロット 8 だけを拾う（Y の 9・直線移動は拾わない）。Live の別セクション（全体で1本）も数える
        assert_eq!(hits.len(), 2, "{hits:?}");
        let x = hits.iter().find(|h| h.track_key == "X").expect("X が無い");
        assert_eq!((x.scene, x.layer, x.frame_b, x.frame_e), (0, 0, 0, 59));
        assert_eq!(x.effect_name, "標準描画");
        let g = hits.iter().find(|h| h.track_key == "強度").expect("強度 が無い");
        // 中間点つき（frame=10,15,20）は先頭と末尾を取る
        assert_eq!((g.scene, g.layer, g.frame_b, g.frame_e), (1, 3, 10, 20));
        assert_eq!(g.section, "全体で1本");

        // スロット 7 は 0 件
        assert!(none.is_empty(), "{none:?}");
        // ファイルが無ければ空（落ちない）
        assert!(search_aup2_file(&PathBuf::from(r"C:\no\such\file.aup2"), &q).is_empty());
    }

    /// 本体の今の状態（オブジェクトエイリアス）から拾う。`get_object_alias` が返す形
    /// （`[Object]` / `[Object.N]`、改行は CRLF のこともある）。
    #[test]
    fn search_alias_text_hits_and_misses() {
        let alias = [
            "[Object]", "frame=0,59",
            "[Object.0]", "effect.name=図形", "サイズ=100",
            "[Object.1]", "effect.name=グレイン@Filters_H",
            "強度=0.0,100.0,全体で1本@カーブエディタ_H,1|2|0,0.511224,1,0",
            "[Object.2]", "effect.name=標準描画",
            "X=0.00,100.00,カーブエディタ_H@カーブエディタ_H,0|3",
            "Y=0.00,100.00,直線移動,0",
        ]
        .join("\r\n");
        let q2 = SearchQuery { section: LIVE_SECTION.to_string(), slot: 2, filter_slot: true };
        let got = search_alias_text(&alias, &q2);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0], ("グレイン@Filters_H".to_string(), "強度".to_string(), "全体で1本".to_string(), 2));
        // スロット 1 は 0 件（12 段目で保存前のファイルに当たったのはこの形）
        let q1 = SearchQuery { section: LIVE_SECTION.to_string(), slot: 1, filter_slot: true };
        assert!(search_alias_text(&alias, &q1).is_empty());
        // 効果名は区切り（[Object.N]）ごとに付け替わる
        let q3 = SearchQuery { section: LIVE_SECTION.to_string(), slot: 3, filter_slot: true };
        let x = search_alias_text(&alias, &q3);
        assert_eq!(x.len(), 1);
        assert_eq!((x[0].0.as_str(), x[0].1.as_str()), ("標準描画", "X"));
    }

    fn hit(scene: i32, layer: i32, key: &str, slot: i32) -> UsageHit {
        UsageHit {
            scene, layer, frame_b: 0, frame_e: 9, effect_name: "e".into(),
            track_key: key.into(), section: LIVE_SECTION.into(), slot,
        }
    }

    /// 今のシーンは本体の状態を正とし、保存済みファイルの同じシーンの結果（古いかもしれない）は捨てる。
    #[test]
    fn merge_prefers_live_for_current_scene() {
        let live = vec![hit(1, 4, "強度", 2)];
        let saved = vec![hit(1, 4, "古い", 2), hit(0, 0, "X", 2), hit(2, 1, "Y", 2)];
        let got = merge_live_and_saved(1, live, saved);
        let keys: Vec<_> = got.iter().map(|h| (h.scene, h.track_key.as_str())).collect();
        // 今のシーンが先頭、ほかのシーンは番号順。今のシーンのファイル側の結果は入らない
        assert_eq!(keys, vec![(1, "強度"), (0, "X"), (2, "Y")]);
    }
}
