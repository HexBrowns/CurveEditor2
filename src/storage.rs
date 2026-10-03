//! CurveEditor2 用 Lua セクションファイル読み書き

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use aviutl2::config;

#[derive(Debug, Clone)]
pub struct SectionFile {
    pub sections: Vec<(String, String)>,
}

impl SectionFile {
    pub fn parse(content: &str) -> Self {
        let content = strip_bom(content.replace('\r', ""));
        let mut sections = Vec::new();
        let mut cur_name = String::new();
        let mut cur_code = String::new();

        // 本文の末尾の改行は 1 つに揃える。`to_string` は節の区切りに空行を 1 行足すので、
        // 揃えないと読み書きの往復ごとに末尾の空行が 1 行ずつ増えていた
        // （issues/20260918_live_slot_trailing_blank_lines_grow.md。実物のスロットに 32〜90 行溜まっていた）
        let mut flush = |name: &str, code: &str| {
            if !name.is_empty() {
                sections.push((name.to_string(), normalize_trailing_newlines(code)));
            }
        };

        for line in content.lines() {
            if !line.is_empty() && line.starts_with('@') {
                flush(&cur_name, &cur_code);
                cur_name = line[1..].to_string();
                cur_code.clear();
            } else if !cur_name.is_empty() {
                cur_code.push_str(line);
                cur_code.push('\n');
            }
        }
        flush(&cur_name, &cur_code);
        Self { sections }
    }

    pub fn to_string(&self) -> String {
        let mut out = String::new();
        for (name, code) in &self.sections {
            if code.trim().is_empty() {
                continue;
            }
            out.push('@');
            out.push_str(name);
            out.push('\n');
            out.push_str(code);
            if !code.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }

    pub fn read(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        Ok(Self::parse(&content))
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, self.to_string())
            .with_context(|| format!("write {}", path.display()))
    }
}

fn strip_bom(s: String) -> String {
    if s.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]) {
        s[3..].to_string()
    } else {
        s
    }
}

pub fn plugin_dir() -> PathBuf {
    config::app_data_path().join("Plugin").join("CurveEditor2")
}

fn legacy_scriptcurve_dir() -> PathBuf {
    config::app_data_path().join("Plugin").join("ScriptCurve")
}

pub fn presets_path() -> PathBuf {
    plugin_dir().join("CurveEditor2Presets.lua")
}

pub fn active_live_path() -> PathBuf {
    plugin_dir().join("CurveEditor2Live_active.lua")
}

/// プロジェクトと同じフォルダの `{プロジェクト名}.{name}`。
///
/// 以前は `name` をそのままフォルダに 1 つ置いていて、同じフォルダのプロジェクトどうしで Live とライブラリが共有された
/// （2026-09-27、`issues/20260927_live_and_library_shared_per_folder.md`）。書くのはこちらだけ
pub fn per_project_path(project_path: &Path, name: &str) -> PathBuf {
    let dir = project_path.parent().map(Path::to_path_buf).unwrap_or_default();
    match project_path.file_stem() {
        Some(stem) => dir.join(format!("{}.{name}", stem.to_string_lossy())),
        None => dir.join(name),
    }
}

/// プロジェクトごとの Live（`{プロジェクト名}.CurveEditor2Live.lua`）
pub fn project_live_path(project_path: &Path) -> PathBuf {
    per_project_path(project_path, "CurveEditor2Live.lua")
}

/// 以前のフォルダに 1 つの Live。読むだけ（プロジェクトごとの Live が無いときに引き継ぐ）。
/// 同じフォルダのほかのプロジェクトも読むので書き換えない
fn folder_live_path(project_path: &Path) -> PathBuf {
    project_path
        .parent()
        .map(|p| p.join("CurveEditor2Live.lua"))
        .unwrap_or_else(|| PathBuf::from("CurveEditor2Live.lua"))
}

fn legacy_presets_path() -> PathBuf {
    legacy_scriptcurve_dir().join("ScriptCurvePresets.lua")
}

fn legacy_active_live_path() -> PathBuf {
    legacy_scriptcurve_dir().join("ScriptCurveLive_active.lua")
}

fn legacy_project_live_path(project_path: &Path) -> PathBuf {
    project_path
        .parent()
        .map(|p| p.join("ScriptCurveLive.lua"))
        .unwrap_or_else(|| PathBuf::from("ScriptCurveLive.lua"))
}

pub fn load_presets() -> Result<Vec<(String, String)>> {
    let path = presets_path();
    if path.exists() {
        return Ok(SectionFile::read(&path)?.sections);
    }
    let legacy = legacy_presets_path();
    if legacy.exists() {
        return Ok(SectionFile::read(&legacy)?.sections);
    }
    Ok(Vec::new())
}

pub fn save_preset(name: &str, code: &str) -> Result<()> {
    let path = presets_path();
    let mut file = if path.exists() {
        SectionFile::read(&path)?
    } else if legacy_presets_path().exists() {
        SectionFile::read(&legacy_presets_path())?
    } else {
        SectionFile {
            sections: Vec::new(),
        }
    };
    if let Some(entry) = file.sections.iter_mut().find(|(n, _)| n == name) {
        entry.1 = code.to_string();
    } else {
        file.sections.push((name.to_string(), code.to_string()));
    }
    file.write(&path)
}

pub fn delete_preset(name: &str) -> Result<()> {
    let path = presets_path();
    if !path.exists() {
        return Ok(());
    }
    let mut file = SectionFile::read(&path)?;
    file.sections.retain(|(n, _)| n != name);
    file.write(&path)
}

pub fn load_live_slots_from(path: &Path) -> Result<HashMap<i32, String>> {
    let mut map = HashMap::new();
    if !path.exists() {
        return Ok(map);
    }
    for (name, code) in SectionFile::read(path)?.sections {
        if let Some(slot_str) = name.strip_prefix(".live.") {
            if let Ok(slot) = slot_str.parse::<i32>() {
                if slot >= 1 {
                    map.insert(slot, code);
                }
            }
        }
    }
    Ok(map)
}

pub fn write_live_slots(path: &Path, slots: &HashMap<i32, String>) -> Result<()> {
    let mut sections: Vec<_> = slots
        .iter()
        .filter(|(_, code)| !code.trim().is_empty())
        .map(|(slot, code)| (format!(".live.{slot}"), code.clone()))
        .collect();
    sections.sort_by(|a, b| {
        let sa: i32 = a.0.strip_prefix(".live.").unwrap_or("0").parse().unwrap_or(0);
        let sb: i32 = b.0.strip_prefix(".live.").unwrap_or("0").parse().unwrap_or(0);
        sa.cmp(&sb)
    });
    let file = SectionFile { sections };
    let new_content = file.to_string();
    // 内容が同一なら mtime を動かさない（起動中の再検知ダイアログ防止）
    if path.exists() {
        if let Ok(existing) = fs::read_to_string(path) {
            if strip_bom(existing.replace('\r', "")) == strip_bom(new_content.replace('\r', "")) {
                return Ok(());
            }
        }
    }
    file.write(path)
}

pub fn sync_live_to_disk(
    project_path: Option<&Path>,
    slots: &HashMap<i32, String>,
) -> Result<()> {
    fs::create_dir_all(plugin_dir())?;
    write_live_slots(&active_live_path(), slots)?;
    if let Some(proj) = project_path {
        if slots.values().any(|c| !c.trim().is_empty()) {
            write_live_slots(&project_live_path(proj), slots)?;
        }
    }
    Ok(())
}

/// プロジェクトを読み込んだときのスロット。
///
/// プロジェクト側の Live があれば、それだけを使う。active は別のプロジェクトで最後に書かれた中身のことがあり、
/// 混ぜるとプロジェクトに無いスロットが紛れ込み、次の保存でプロジェクト側に書かれてしまう
/// （2026-09-27、`issues/20260927_active_live_differs_from_saved_project.md`）。
/// プロジェクト側に無いとき（新しいプロジェクト・パスなし）だけ active を引き継ぐ。
///
/// 読む順: プロジェクトごとの Live → フォルダに 1 つの Live（以前の形。引き継ぐだけで書かない）→ ScriptCurve の Live → active
pub fn load_live_for_project(project_path: Option<&Path>) -> Result<HashMap<i32, String>> {
    if let Some(proj) = project_path {
        let proj_path = project_live_path(proj);
        if proj_path.exists() {
            return load_live_slots_from(&proj_path);
        }
        let folder = folder_live_path(proj);
        if folder.exists() {
            return load_live_slots_from(&folder);
        }
        let legacy = legacy_project_live_path(proj);
        if legacy.exists() {
            return load_live_slots_from(&legacy);
        }
    }

    let mut merged = HashMap::new();
    if active_live_path().exists() {
        merge_live_slots(&mut merged, &load_live_slots_from(&active_live_path())?);
    } else if legacy_active_live_path().exists() {
        merge_live_slots(&mut merged, &load_live_slots_from(&legacy_active_live_path())?);
    }
    Ok(merged)
}

/// active（`@カーブエディタ2.tra2` が読む唯一のファイル）だけを書く。プロジェクトを読み込んだ直後に呼び、
/// 読み込んだプロジェクトのカーブで描画されるようにする。中身が同じなら書かない（`write_live_slots`）
pub fn write_active_live(slots: &HashMap<i32, String>) -> Result<()> {
    fs::create_dir_all(plugin_dir())?;
    write_live_slots(&active_live_path(), slots)
}

/// 外から active に書かれたスロットを `slots` に取り込む（3 者の突き合わせ）。戻り値は取り込んだスロット番号。
///
/// `base` はこちらが前回 active に書いた（読んだ）中身、`disk` は今の active。`disk` と `base` が違うスロットは
/// 外で足された・書き換えられた・消されたもの（ClaudeBridge の `set_custom_curve`、`verify_slots.py` など）。
/// そのスロットをメモリ（`slots`）が `base` から変えていなければ外の中身にする。両方が変えていればメモリを残す
/// （ユーザーがいま反映したもの）。本文は末尾の改行を揃えて比べる（ファイルに書くと揃うため）。
///
/// これをせずに書くと、外から足したスロットが次の保存（1 分ごとの自動バックアップを含む）で消え、
/// そのスロットを指すトラックが黙って別の動きになる（2026-09-27、ClaudeBridge の実機確認で 28 秒後に消えた。
/// `issues/20260927_external_live_slots_lost_on_save.md`）
pub fn adopt_external_live(
    slots: &mut HashMap<i32, String>,
    base: &HashMap<i32, String>,
    disk: &HashMap<i32, String>,
) -> Vec<i32> {
    fn norm(v: Option<&String>) -> Option<String> {
        v.map(|s| normalize_trailing_newlines(s)).filter(|s| !s.trim().is_empty())
    }
    let mut keys: Vec<i32> = base.keys().chain(disk.keys()).copied().collect();
    keys.sort_unstable();
    keys.dedup();
    let mut adopted = Vec::new();
    for k in keys {
        let theirs = norm(disk.get(&k));
        let old = norm(base.get(&k));
        if theirs == old || norm(slots.get(&k)) != old {
            continue;
        }
        match disk.get(&k) {
            Some(code) if theirs.is_some() => {
                slots.insert(k, code.clone());
            }
            _ => {
                slots.remove(&k);
            }
        }
        adopted.push(k);
    }
    adopted
}

/// 外から書かれたスロットを取り込んでから `sync_live_to_disk` し、`base` を書いた後の active にする。
pub fn sync_live_to_disk_adopting(
    project_path: Option<&Path>,
    slots: &mut HashMap<i32, String>,
    base: &mut HashMap<i32, String>,
) -> Result<Vec<i32>> {
    let disk = load_live_slots_from(&active_live_path())?;
    let adopted = adopt_external_live(slots, base, &disk);
    sync_live_to_disk(project_path, slots)?;
    *base = read_active_live();
    Ok(adopted)
}

/// 今の active の中身（`adopt_external_live` の base にする）。読めなければ空
pub fn read_active_live() -> HashMap<i32, String> {
    load_live_slots_from(&active_live_path()).unwrap_or_default()
}

fn merge_live_slots(dst: &mut HashMap<i32, String>, src: &HashMap<i32, String>) {
    for (k, v) in src {
        dst.insert(*k, v.clone());
    }
}

pub fn category_of(name: &str) -> (&str, &str) {
    if let Some((cat, leaf)) = name.split_once('/') {
        (cat, leaf)
    } else {
        ("", name)
    }
}

/// 本文の末尾の改行を 1 つに揃える（空の本文は空のまま）。途中の空行と、空白だけの行には触らない。
fn normalize_trailing_newlines(code: &str) -> String {
    let body = code.trim_end_matches('\n');
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

#[cfg(test)]
mod adopt_tests {
    use super::adopt_external_live;
    use std::collections::HashMap;

    fn m(v: &[(i32, &str)]) -> HashMap<i32, String> {
        v.iter().map(|(k, s)| (*k, s.to_string())).collect()
    }

    #[test]
    fn slot_added_outside_is_adopted() {
        // ClaudeBridge が 9101 を足した。こちらは触っていない
        let base = m(&[(1, "a\n")]);
        let disk = m(&[(1, "a\n"), (9101, "bez\n")]);
        let mut mine = m(&[(1, "a")]); // 末尾の改行が無くても同じ中身として扱う
        assert_eq!(adopt_external_live(&mut mine, &base, &disk), vec![9101]);
        assert_eq!(mine[&9101], "bez\n");
        assert_eq!(mine[&1], "a");
    }

    #[test]
    fn slot_changed_or_removed_outside_is_adopted() {
        let base = m(&[(1, "a"), (2, "b")]);
        let disk = m(&[(1, "A")]);
        let mut mine = base.clone();
        assert_eq!(adopt_external_live(&mut mine, &base, &disk), vec![1, 2]);
        assert_eq!(mine, m(&[(1, "A")]));
    }

    #[test]
    fn local_change_wins_over_outside_change() {
        // 同じスロットを両方が変えた（ユーザーがいま反映した）→ メモリを残す
        let base = m(&[(1, "a")]);
        let disk = m(&[(1, "outside")]);
        let mut mine = m(&[(1, "mine")]);
        assert!(adopt_external_live(&mut mine, &base, &disk).is_empty());
        assert_eq!(mine[&1], "mine");
    }

    #[test]
    fn local_edits_are_kept_when_disk_is_unchanged() {
        // 外は何もしていない。メモリで消した・足したものはそのまま（直す前と同じ動き）
        let base = m(&[(1, "a"), (2, "b")]);
        let disk = base.clone();
        let mut mine = m(&[(1, "a"), (3, "c")]);
        assert!(adopt_external_live(&mut mine, &base, &disk).is_empty());
        assert_eq!(mine, m(&[(1, "a"), (3, "c")]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// プロジェクト側の Live があれば、それだけを読む（active を混ぜない）。
    /// 以前は active を先に読んでプロジェクト側で上書きしていて、プロジェクトに無いスロットが紛れ込んだ
    /// （issues/20260927_active_live_differs_from_saved_project.md）。
    /// active の場所はホストの設定で決まるのでテストでは作れない。プロジェクト側がある分岐は active に触らずに返す
    #[test]
    fn project_live_is_used_alone() {
        let dir = std::env::temp_dir().join(format!("ce2_live_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let project = dir.join("作品.aup2");
        fs::write(dir.join("CurveEditor2Live.lua"), "@.live.3\nreturn t\n\n@.live.4\nreturn 1 - t\n\n").unwrap();
        let slots = load_live_for_project(Some(&project)).unwrap();
        let mut keys: Vec<_> = slots.keys().copied().collect();
        keys.sort();
        assert_eq!(keys, vec![3, 4], "プロジェクト側のスロットだけ");
        assert_eq!(slots[&3], "return t\n");
        fs::remove_dir_all(&dir).ok();
    }

    /// Live とライブラリはプロジェクトごとのファイル名（`{プロジェクト名}.…`）。
    /// 同じフォルダの 2 つのプロジェクトが別のファイルを持ち、どちらもフォルダに 1 つの以前のファイルより優先される
    /// （issues/20260927_live_and_library_shared_per_folder.md）
    #[test]
    fn per_project_files_are_separate_and_preferred() {
        let dir = std::env::temp_dir().join(format!("ce2_per_project_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("作品A.aup2");
        let b = dir.join("作品B.aup2");
        assert_eq!(project_live_path(&a), dir.join("作品A.CurveEditor2Live.lua"));
        assert_eq!(
            crate::curve_library::library_file_path(Some(&a)),
            Some(dir.join("作品A.CurveEditor2Library.json"))
        );
        // 以前の形（フォルダに 1 つ）だけがあるときは、どちらのプロジェクトもそれを引き継ぐ
        fs::write(dir.join("CurveEditor2Live.lua"), "@.live.1\nreturn t\n\n").unwrap();
        fs::write(dir.join("CurveEditor2Library.json"), "{\"curves\":{}}").unwrap();
        assert_eq!(load_live_for_project(Some(&a)).unwrap()[&1], "return t\n");
        assert_eq!(
            crate::curve_library::library_file_to_load(Some(&b)),
            Some(dir.join("CurveEditor2Library.json"))
        );
        // A だけがプロジェクトごとの Live を持つと、A はそれを、B は以前のものを読む
        write_live_slots(&project_live_path(&a), &HashMap::from([(1, "return 1 - t\n".to_string())])).unwrap();
        assert_eq!(load_live_for_project(Some(&a)).unwrap()[&1], "return 1 - t\n");
        assert_eq!(load_live_for_project(Some(&b)).unwrap()[&1], "return t\n");
        fs::write(dir.join("作品B.CurveEditor2Library.json"), "{\"curves\":{}}").unwrap();
        assert_eq!(
            crate::curve_library::library_file_to_load(Some(&b)),
            Some(dir.join("作品B.CurveEditor2Library.json"))
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// 読み書きを何度往復しても変わらない（以前は往復ごとに末尾の空行が 1 行増えた）。
    #[test]
    fn roundtrip_is_stable() {
        let src = "@.live.1\nreturn t\n\n@.live.2\n-- 途中の空行は残す\nlocal k = 2\n\nreturn t ^ k\n\n";
        let once = SectionFile::parse(src).to_string();
        assert_eq!(once, src, "エディタが書く形はそのまま読み書きできる");
        let mut text = once.clone();
        for _ in 0..5 {
            text = SectionFile::parse(&text).to_string();
        }
        assert_eq!(text, once);
    }

    /// すでに溜まった空行は、1 回読み書きすれば 1 行に戻る。式の中身は変わらない。
    #[test]
    fn accumulated_blank_lines_collapse() {
        let grown = format!("@.live.1\nreturn t{}@.live.10\nreturn 1 - t{}", "\n".repeat(33), "\n".repeat(91));
        let file = SectionFile::parse(&grown);
        assert_eq!(
            file.sections,
            vec![
                (".live.1".to_string(), "return t\n".to_string()),
                (".live.10".to_string(), "return 1 - t\n".to_string()),
            ]
        );
        assert_eq!(file.to_string(), "@.live.1\nreturn t\n\n@.live.10\nreturn 1 - t\n\n");
    }

    /// CRLF・空白だけの行・本文が空の節。
    #[test]
    fn edge_cases_are_stable() {
        let file = SectionFile::parse("@.live.3\r\nreturn t\r\n  \r\n\r\n\r\n@.live.4\r\n\r\n\r\n");
        assert_eq!(file.sections[0].1, "return t\n  \n", "空白だけの行は式の一部として残す");
        assert_eq!(file.sections[1].1, "", "空の節は空のまま（to_string が書かない）");
        let text = file.to_string();
        assert_eq!(text, "@.live.3\nreturn t\n  \n\n");
        assert_eq!(SectionFile::parse(&text).to_string(), text);
    }

    /// Live の書き込みは内容が同じなら書き直さない（mtime を動かさない）。往復で変わらなくなったので、
    /// 2 回目以降の同期でファイルが書き換わらない。
    #[test]
    fn write_live_slots_is_idempotent_on_disk() {
        let dir = std::env::temp_dir().join(format!("ce2_storage_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("live.lua");
        // 空行が溜まった実物の形から始める
        fs::write(&path, format!("@.live.1\nreturn t{}@.live.2\nreturn t * t{}", "\n".repeat(30), "\n".repeat(60))).unwrap();
        let slots = load_live_slots_from(&path).unwrap();
        write_live_slots(&path, &slots).unwrap();
        let first = fs::read(&path).unwrap();
        for _ in 0..3 {
            let slots = load_live_slots_from(&path).unwrap();
            write_live_slots(&path, &slots).unwrap();
        }
        assert_eq!(fs::read(&path).unwrap(), first);
        assert_eq!(String::from_utf8(first).unwrap(), "@.live.1\nreturn t\n\n@.live.2\nreturn t * t\n\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
