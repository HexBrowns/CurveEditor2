//! `aviutl2.ini` への Movement 登録（`findMovement` クラッシュ防止）
//!
//! トラックバー移動方法は UI から一度選ぶと `[Movement.*]` が ini に追加されるが、
//! EditHandle 経由で直接設定した場合は未登録だと AviUtl2 が例外で落ちる。
//! さらに ini への追記だけではメモリ上の Movement 一覧は更新されないため、
//! 新規追記時は AviUtl2 の再起動が必要。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use aviutl2::config;

use crate::search::{BUILTIN_SECTIONS, LIVE_SECTION, MOTION_LABEL};

const INI_NAME: &str = "aviutl2.ini";
const ORDER_BASE: i32 = 900;

/// ini 追記直後に UI／エラーで共有する再起動案内。
pub const RESTART_REQUIRED_MESSAGE: &str =
    "移動方法を aviutl2.ini に登録しました。AviUtl2 を再起動してから再度「このトラックに適用」してください。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionRegisterStatus {
    /// 既に正しい形式で登録済み
    Present,
    /// ini に追記した（再起動が必要）
    Added,
}

pub fn ini_path() -> PathBuf {
    config::app_data_path().join(INI_NAME)
}

/// AviUtl2 が期待する Movement キー（トラック値の移動方法フィールドと同一）。
/// 例: Live `カーブエディタ2@カーブエディタ2`、組み込み `ベジェ@カーブエディタ2`
pub fn motion_ini_key(section: &str) -> String {
    if section == LIVE_SECTION {
        format!("{MOTION_LABEL}@{MOTION_LABEL}")
    } else {
        format!("{section}@{MOTION_LABEL}")
    }
}

/// v0.6.1 以前の誤った Bracket 形式（互換検出用）。
fn legacy_motion_ini_key(section: &str) -> String {
    if section == LIVE_SECTION {
        motion_ini_key(section)
    } else {
        format!("{MOTION_LABEL}[{section}]@{MOTION_LABEL}")
    }
}

/// 未登録の CurveEditor2 Movement エントリを `aviutl2.ini` に追記する。
pub fn ensure_movements_registered() -> Result<usize> {
    let path = ini_path();
    if !path.exists() {
        return Ok(0);
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut out = content.clone();
    if !out.ends_with('\n') {
        out.push('\n');
    }

    let mut order = next_order(&content);
    let mut added = 0;

    added += append_section_if_missing(&mut out, LIVE_SECTION, &mut order)?;
    for section in BUILTIN_SECTIONS {
        if *section == LIVE_SECTION {
            continue;
        }
        added += append_section_if_missing(&mut out, section, &mut order)?;
    }

    if added > 0 {
        fs::write(&path, &out).with_context(|| format!("write {}", path.display()))?;
    }

    Ok(added)
}

/// 指定セクションの Movement が未登録なら追記する。
pub fn ensure_section_registered(section: &str) -> Result<MotionRegisterStatus> {
    let path = ini_path();
    if !path.exists() {
        return Ok(MotionRegisterStatus::Present);
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let correct = motion_ini_key(section);
    if content.contains(&movement_header(&correct)) {
        return Ok(MotionRegisterStatus::Present);
    }

    let mut out = content;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let mut order = next_order(&out);
    let added = append_section_if_missing(&mut out, section, &mut order)?;
    if added > 0 {
        fs::write(&path, &out).with_context(|| format!("write {}", path.display()))?;
        Ok(MotionRegisterStatus::Added)
    } else {
        Ok(MotionRegisterStatus::Present)
    }
}

fn movement_header(motion_key: &str) -> String {
    format!("[Movement.{motion_key}]")
}

fn next_order(content: &str) -> i32 {
    let mut max_order = ORDER_BASE;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("order=") {
            if let Ok(n) = rest.trim().parse::<i32>() {
                max_order = max_order.max(n);
            }
        }
    }
    max_order + 1
}

fn append_section_if_missing(out: &mut String, section: &str, order: &mut i32) -> Result<usize> {
    let motion_key = motion_ini_key(section);
    let header = movement_header(&motion_key);
    if out.contains(&header) {
        return Ok(0);
    }

    let legacy = legacy_motion_ini_key(section);
    if legacy != motion_key && out.contains(&movement_header(&legacy)) {
        // 誤形式のみ存在 → 正しい形式を追記
    }

    let label_line = if section == LIVE_SECTION {
        "label=\n".to_string()
    } else {
        format!("label={MOTION_LABEL}\n")
    };
    out.push_str(&format!("{header}\n{label_line}hide=0\norder={}\n", *order));
    *order += 1;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_ini_key_live() {
        assert_eq!(motion_ini_key(LIVE_SECTION), "カーブエディタ2@カーブエディタ2");
    }

    #[test]
    fn motion_ini_key_builtin() {
        assert_eq!(motion_ini_key("ベジェ"), "ベジェ@カーブエディタ2");
    }

    #[test]
    fn legacy_motion_ini_key_builtin() {
        assert_eq!(
            legacy_motion_ini_key("ベジェ"),
            "カーブエディタ2[ベジェ]@カーブエディタ2"
        );
    }
}
