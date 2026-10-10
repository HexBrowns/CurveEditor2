//! カーブエディタ_H の移動方法が本体に登録されているかを確かめる（`not found movement` の防止）
//!
//! 本体に登録されていない移動方法の名前をトラックに書くと、本体が例外 `not found movement`
//! （`EffectService::findMovement`）を出し、`call_edit_section` のコールバックが途中で打ち切られる。
//! 戻り値で失敗を知る手段が無いので、書く前に名前が登録されているかを確かめる。
//!
//! **`aviutl2.ini` の `[Movement.*]` は読みも書きもしない**（v0.9.11 まではここへ直接追記していた）:
//! - 本体は移動方法を起動時に `.tra2` から登録する。ini に追記しても登録は増えない（`.tra2` が無ければ再起動しても無い）
//! - ini には消したスクリプトの名前が残り続けるので、ini にあることは登録されている証拠にならない
//!   （.tra2 v1.10 で外した `加減速@カーブエディタ_H` も本番の ini に残っている）
//! - 本体は起動中に ini を持っていて、終了時に書き戻す。起動中の追記は消えうる
//!
//! そこで、本体が読む場所（`Script/` とその一つ下）の `.tra2` から名前を作る（ルール `au2-rs-plugin`「設定項目の値の形式」、
//! 参照実装 `AI/plugins/MidpointTable_H/src/movement.rs`）。本体は `.tra2` を起動時に読むので、
//! プラグインが読み込まれた時刻より新しいファイルの名前は「再起動すれば使える」として分ける。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use aviutl2::config;

use crate::search::{BUILTIN_SECTIONS, LIVE_SECTION, MOTION_LABEL};

/// プラグインが読み込まれた時刻（本体がスクリプトを読んだ時刻の代わり）
static LOADED_AT: OnceLock<SystemTime> = OnceLock::new();

/// プラグインの読み込み時に 1 回呼ぶ。これより新しい `.tra2` は本体に登録されていないとみなす
pub fn mark_loaded() {
    let _ = LOADED_AT.set(SystemTime::now());
}

/// AviUtl2 が期待する移動方法の名前（トラック値の移動方法の欄と同じ）。
/// 例: Live `カーブエディタ_H@カーブエディタ_H`、組み込み `ベジェ@カーブエディタ_H`
pub fn motion_ini_key(section: &str) -> String {
    if section == LIVE_SECTION {
        format!("{MOTION_LABEL}@{MOTION_LABEL}")
    } else {
        format!("{section}@{MOTION_LABEL}")
    }
}

/// `.tra2` から作った移動方法の名前。
#[derive(Debug, Default)]
pub struct Tra2Names {
    /// 本体が起動時に読んだはずのファイルの名前（登録されている）
    pub registered: HashSet<String>,
    /// 起動した後に置かれた・更新されたファイルの名前（再起動するまで登録されない）
    pub pending: HashSet<String>,
}

/// 1 ファイルから名前を作る。`@名前.tra2` は行頭の `@セクション` ごとに `セクション@名前`、ほかはファイル名
fn names_in_file(path: &Path, text: &str) -> Vec<String> {
    let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let Some(stem) = file.strip_suffix(".tra2") else {
        return Vec::new();
    };
    match stem.strip_prefix('@') {
        Some(base) => text
            .lines()
            .filter_map(|l| l.trim_end_matches('\r').strip_prefix('@'))
            .map(|s| format!("{s}@{base}"))
            .collect(),
        None => vec![stem.to_string()],
    }
}

/// `Script/` 直下と、その一つ下のフォルダの `.tra2`（本体が読むのはここまで）
fn tra2_files(script_dir: &Path) -> Vec<PathBuf> {
    let is_tra2 = |p: &Path| p.is_file() && p.extension().is_some_and(|x| x == "tra2");
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(script_dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&p) {
                out.extend(sub.flatten().map(|e| e.path()).filter(|p| is_tra2(p)));
            }
        } else if is_tra2(&p) {
            out.push(p);
        }
    }
    out
}

/// `script_dir` の `.tra2` から名前を集める。`loaded_at` より新しいファイルは `pending` に入れる
pub fn scan(script_dir: &Path, loaded_at: Option<SystemTime>) -> Tra2Names {
    let mut names = Tra2Names::default();
    for f in tra2_files(script_dir) {
        let newer = loaded_at.is_some_and(|t| {
            std::fs::metadata(&f)
                .and_then(|m| m.modified())
                .is_ok_and(|m| m > t)
        });
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        let found = names_in_file(&f, &String::from_utf8_lossy(&bytes));
        if newer {
            names.pending.extend(found);
        } else {
            names.registered.extend(found);
        }
    }
    names
}

/// 本番の `Script/` を読む
pub fn scan_installed() -> Tra2Names {
    scan(&config::app_data_path().join("Script"), LOADED_AT.get().copied())
}

/// 登録されていない理由（`None` なら登録されている）
pub fn unregistered_reason(names: &Tra2Names, section: &str) -> Option<String> {
    let key = motion_ini_key(section);
    if names.registered.contains(&key) {
        return None;
    }
    Some(if names.pending.contains(&key) {
        format!(
            "移動方法「{key}」は AviUtl2 を起動した後に置かれた（更新された）.tra2 にあり、まだ本体に登録されていません。AviUtl2 を再起動してから、もう一度「このトラックに適用」してください。"
        )
    } else {
        format!(
            "移動方法「{key}」が本体に登録されていません。Script/CurveEditor_H/@{MOTION_LABEL}.tra2 を最新の版で置き直し、AviUtl2 を再起動してください。"
        )
    })
}

/// 適用する前に、その移動方法が登録されているかを確かめる（`.tra2` を読む。ini には触れない）
pub fn ensure_section_registered(section: &str) -> Result<(), String> {
    match unregistered_reason(&scan_installed(), section) {
        None => Ok(()),
        Some(reason) => Err(reason),
    }
}

/// カーブエディタ_H の移動方法（Live と組み込み）のうち、登録されていないものの名前
pub fn missing_movements(names: &Tra2Names) -> Vec<String> {
    std::iter::once(LIVE_SECTION)
        .chain(BUILTIN_SECTIONS.iter().copied().filter(|s| *s != LIVE_SECTION))
        .map(motion_ini_key)
        .filter(|key| !names.registered.contains(key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_ini_key_live() {
        assert_eq!(motion_ini_key(LIVE_SECTION), "カーブエディタ_H@カーブエディタ_H");
    }

    #[test]
    fn motion_ini_key_builtin() {
        assert_eq!(motion_ini_key("ベジェ"), "ベジェ@カーブエディタ_H");
    }

    #[test]
    fn names_from_files() {
        let multi = names_in_file(
            Path::new("Script/CurveEditor_H/@カーブエディタ_H.tra2"),
            "--label:x\r\n@カーブエディタ_H\r\n--param:a,1\r\n@ベジェ\r\n",
        );
        assert_eq!(multi, ["カーブエディタ_H@カーブエディタ_H", "ベジェ@カーブエディタ_H"]);
        assert_eq!(names_in_file(Path::new("Script/バウンス.tra2"), "@x\n"), ["バウンス"]);
        assert!(names_in_file(Path::new("Script/x.anm2"), "@A\n").is_empty());
    }

    fn temp_script_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("curveeditor2_movement_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("CurveEditor_H").join("too_deep")).unwrap();
        dir
    }

    #[test]
    fn scan_reads_script_and_one_level_below_only() {
        let dir = temp_script_dir("depth");
        std::fs::write(dir.join("CurveEditor_H").join("@カーブエディタ_H.tra2"), "@カーブエディタ_H\n@ベジェ\n").unwrap();
        std::fs::write(dir.join("直下.tra2"), "").unwrap();
        std::fs::write(dir.join("CurveEditor_H").join("too_deep").join("@深い.tra2"), "@A\n").unwrap();
        let names = scan(&dir, None);
        assert!(names.registered.contains("ベジェ@カーブエディタ_H"));
        assert!(names.registered.contains("直下"));
        assert!(!names.registered.contains("A@深い"), "2 階層下は本体が読まない");
        assert!(unregistered_reason(&names, LIVE_SECTION).is_none());
        assert!(unregistered_reason(&names, "ベジェ").is_none());
        let reason = unregistered_reason(&names, "サイン").expect("無い名前");
        assert!(reason.contains("サイン@カーブエディタ_H") && reason.contains("登録されていません"), "{reason}");
        let missing = missing_movements(&names);
        assert!(missing.contains(&"サイン@カーブエディタ_H".to_string()));
        assert!(!missing.contains(&"ベジェ@カーブエディタ_H".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_newer_than_load_need_restart() {
        let dir = temp_script_dir("newer");
        std::fs::write(dir.join("CurveEditor_H").join("@カーブエディタ_H.tra2"), "@ベジェ\n").unwrap();
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        let names = scan(&dir, Some(past));
        assert!(names.registered.is_empty());
        assert!(names.pending.contains("ベジェ@カーブエディタ_H"));
        let reason = unregistered_reason(&names, "ベジェ").expect("再起動まで登録されない");
        assert!(reason.contains("再起動"), "{reason}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本番の .tra2 から、カーブエディタ_H の移動方法（Live と組み込み）が全部作れる（本番の無い環境では飛ばす）
    #[test]
    fn production_tra2_has_all_movements() {
        let dir = PathBuf::from(r"C:\ProgramData\aviutl2\Script");
        if !dir.join("CurveEditor_H").exists() {
            return;
        }
        let names = scan(&dir, None);
        assert!(missing_movements(&names).is_empty(), "{:?}", missing_movements(&names));
    }
}
