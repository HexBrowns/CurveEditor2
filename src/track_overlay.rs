//! フォーカスオブジェクトのトラックバー上に編集中カーブを重ねたプレビュー

use aviutl2::generic::TrackInfo;

use crate::curves::{self, BezierParams, CompositeCurve, CurveMode, SpringParams};
use crate::edit_ops::TrackTarget;

pub const OVERLAY_SAMPLE_STEPS: usize = 64;

#[derive(Debug, Clone)]
pub struct TrackOverlayData {
    pub track_label: String,
    pub start_value: f64,
    pub end_value: f64,
    pub frame_start: usize,
    pub frame_end: usize,
    pub motion_mode: Option<String>,
    /// 実測カーブ（キャッシュ表示では未使用）
    pub actual_samples: Vec<(f64, f64)>,
}

/// トラック取得時にキャッシュした値からオーバーレイを構築する（Edit API 不要）。
pub fn build_overlay_from_cache(target: &TrackTarget) -> Result<TrackOverlayData, String> {
    let (start_value, end_value, motion_mode) =
        resolve_track_endpoints(&target.track_value, None).ok_or_else(|| {
            format!(
                "トラック値を解析できません: {}",
                target.track_value
            )
        })?;

    if !start_value.is_finite() || !end_value.is_finite() {
        return Err(format!(
            "トラック値が数値ではありません: start={start_value}, end={end_value}"
        ));
    }

    Ok(TrackOverlayData {
        track_label: target.label.clone(),
        start_value,
        end_value,
        frame_start: target.frame_start,
        frame_end: target.frame_end,
        motion_mode,
        actual_samples: Vec::new(),
    })
}

/// トラックバー値 `開始,終了,移動方法,...` または単一の数値を解析する。
pub fn parse_track_value(value: &str) -> Option<(f64, f64, Option<String>)> {
    let trimmed = value.trim();
    let mut parts = trimmed.splitn(4, ',');
    let first = parts.next()?.trim();
    let second = parts.next();
    if let Some(second) = second {
        let start: f64 = first.parse().ok()?;
        let end: f64 = second.trim().parse().ok()?;
        if !start.is_finite() || !end.is_finite() {
            return None;
        }
        let motion = parts
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        return Some((start, end, motion));
    }
    let v: f64 = first.parse().ok()?;
    if v.is_finite() {
        Some((v, v, None))
    } else {
        None
    }
}

/// エフェクト項目文字列または TrackInfo から開始・終了値を決定する。
pub fn resolve_track_endpoints(
    value: &str,
    track_info: Option<&TrackInfo>,
) -> Option<(f64, f64, Option<String>)> {
    if let Some(parsed) = parse_track_value(value) {
        return Some(parsed);
    }
    let info = track_info?;
    // aviutl2 0.46.1 から `mode` は Option（移動無しは None）。空文字は 0.40 の「移動無し」と同じに扱う
    let motion = info.mode.clone().filter(|m| !m.is_empty());
    match info.params.len() {
        0 => None,
        1 => {
            let v = info.params[0];
            if v.is_finite() {
                Some((v, v, motion))
            } else {
                None
            }
        }
        _ => {
            let start = info.params[0];
            let end = info.params[info.params.len() - 1];
            if start.is_finite() && end.is_finite() {
                Some((start, end, motion))
            } else {
                None
            }
        }
    }
}

/// 編集中カーブをトラックの開始・終了値にマッピングしたサンプル列を返す。
/// スクリプトモード時は `script` の既知形式から近似イージングを使う。
pub fn preview_samples(
    mode: CurveMode,
    bezier: &BezierParams,
    spring: &SpringParams,
    multi: &[curves::Anchor],
    composite: &CompositeCurve,
    script: &str,
    start_value: f64,
    end_value: f64,
    steps: usize,
) -> Vec<(f64, f64)> {
    let span = end_value - start_value;
    let script_preview = if mode == CurveMode::Script {
        curves::detect_script_preview(script)
    } else {
        None
    };
    (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let eased = if let Some(ref preview) = script_preview {
                preview.eval(t)
            } else {
                curves::eval_curve(mode, t, bezier, spring, multi, composite)
            };
            (t, start_value + span * eased)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2::generic::TrackInfo;

    fn sample_target(value: &str) -> TrackTarget {
        TrackTarget {
            effect_name: "標準".into(),
            effect_index: 0,
            alias_index: 0,
            item_name: "中心X".into(),
            label: "標準 / 中心X".into(),
            track_value: value.into(),
            frame_start: 0,
            frame_end: 100,
        }
    }

    #[test]
    fn overlay_from_cached_track_value() {
        let data = build_overlay_from_cache(&sample_target("10,90,移動なし,0")).unwrap();
        assert!((data.start_value - 10.0).abs() < 1e-9);
        assert!((data.end_value - 90.0).abs() < 1e-9);
        assert_eq!(data.frame_end, 100);
    }

    #[test]
    fn parse_track_value_motion() {
        let (s, e, m) =
            parse_track_value("100,200,カーブエディタ_H[バウンス]@カーブエディタ_H,0").unwrap();
        assert!((s - 100.0).abs() < 1e-9);
        assert!((e - 200.0).abs() < 1e-9);
        assert_eq!(m.as_deref(), Some("カーブエディタ_H[バウンス]@カーブエディタ_H"));
    }

    #[test]
    fn resolve_from_track_info_params() {
        let info = TrackInfo {
            mode: Some("ベジェ".into()),
            params: vec![10.0, 50.0, 200.0],
            accelerate: false,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
            group_num: 1,
            group_index: 0,
            group_name: None,
        };
        let (s, e, m) = resolve_track_endpoints("invalid", Some(&info)).unwrap();
        assert!((s - 10.0).abs() < 1e-9);
        assert!((e - 200.0).abs() < 1e-9);
        assert_eq!(m.as_deref(), Some("ベジェ"));

        // 移動無し（0.46.1 から mode が None）は移動方法なしとして読む
        let fixed = TrackInfo { mode: None, params: vec![42.0], ..info };
        let (s, e, m) = resolve_track_endpoints("invalid", Some(&fixed)).unwrap();
        assert!((s - 42.0).abs() < 1e-9 && (e - 42.0).abs() < 1e-9);
        assert_eq!(m, None);
    }

    #[test]
    fn preview_maps_endpoints() {
        let bezier = BezierParams::default();
        let spring = SpringParams::default();
        let multi = curves::default_multi_anchors();
        let composite = CompositeCurve::default();
        let samples = preview_samples(
            CurveMode::Bezier,
            &bezier,
            &spring,
            &multi,
            &composite,
            "",
            10.0,
            110.0,
            8,
        );
        assert!((samples[0].1 - 10.0).abs() < 1e-6);
        assert!((samples.last().unwrap().1 - 110.0).abs() < 1e-3);
    }
}
