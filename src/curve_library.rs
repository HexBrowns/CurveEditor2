//! カーブライブラリ（ID 1〜524288）のシリアライズと永続化

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const MAX_CURVE_ID: u32 = 524_288;
pub const PROJECT_SERDE_KEY: &str = "curveeditor2/library";

// 版を上げて項目を足しても前の版のライブラリを読めるように、構造体ごとに `#[serde(default)]` を付ける
// （足りない項目は既定値になる。ルール `au2-plugin-checklist`「設定ファイルの下位互換」）。

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AnchorDoc {
    pub t: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BezierDoc {
    #[serde(default)]
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    #[serde(default = "default_y3")]
    pub y3: f64,
}

fn default_y3() -> f64 {
    1.0
}

impl Default for BezierDoc {
    fn default() -> Self {
        Self { y0: 0.0, x1: 0.25, y1: 0.1, x2: 0.25, y2: 1.0, y3: default_y3() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SpringDoc {
    pub zeta: f64,
    pub omega: f64,
}

impl Default for SpringDoc {
    fn default() -> Self {
        Self { zeta: 0.4, omega: 10.0 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CompositeSegmentDoc {
    pub kind: String,
    pub cx1: f64,
    pub cy1: f64,
    pub cx2: f64,
    pub cy2: f64,
    pub zeta: f64,
    pub omega: f64,
}

/// エディタ状態のスナップショット（ライブラリ保存用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CurveDocument {
    pub mode: String,
    pub script_code: String,
    pub bezier: BezierDoc,
    pub spring: SpringDoc,
    pub multi_anchors: Vec<AnchorDoc>,
    pub composite_anchors: Vec<AnchorDoc>,
    pub composite_segments: Vec<CompositeSegmentDoc>,
    pub selected_segment: usize,
}

impl Default for CurveDocument {
    fn default() -> Self {
        Self {
            mode: "bezier".to_string(),
            script_code: String::new(),
            bezier: BezierDoc::default(),
            spring: SpringDoc::default(),
            multi_anchors: Vec::new(),
            composite_anchors: Vec::new(),
            composite_segments: Vec::new(),
            selected_segment: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CurveLibrary {
    pub curves: HashMap<u32, CurveDocument>,
}

impl CurveLibrary {
    pub fn get(&self, id: u32) -> Option<&CurveDocument> {
        self.curves.get(&id)
    }

    pub fn insert(&mut self, id: u32, doc: CurveDocument) {
        if (1..=MAX_CURVE_ID).contains(&id) {
            self.curves.insert(id, doc);
        }
    }

    pub fn remove(&mut self, id: u32) -> bool {
        self.curves.remove(&id).is_some()
    }

    pub fn defined_count(&self) -> usize {
        self.curves.len()
    }

    pub fn sorted_ids(&self) -> Vec<u32> {
        let mut ids: Vec<_> = self.curves.keys().copied().collect();
        ids.sort_unstable();
        ids
    }
}

/// プロジェクトごとのライブラリ（`{プロジェクト名}.CurveEditor2Library.json`）。書くのはこちらだけ。
/// 以前はフォルダに 1 つで、同じフォルダのプロジェクトどうしで混ざった（`storage::per_project_path`）
pub fn library_file_path(project_path: Option<&Path>) -> Option<PathBuf> {
    project_path.map(|p| crate::storage::per_project_path(p, "CurveEditor2Library.json"))
}

/// 読み込むライブラリのファイル。プロジェクトごとのものが無ければ、フォルダに 1 つの以前のものを引き継ぐ（書かない）
pub fn library_file_to_load(project_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = library_file_path(project_path).filter(|p| p.exists()) {
        return Some(p);
    }
    project_path
        .and_then(|p| p.parent())
        .map(|dir| dir.join("CurveEditor2Library.json"))
        .filter(|p| p.exists())
}

pub fn load_library_file(path: &Path) -> Result<CurveLibrary> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read library {}", path.display()))?;
    let lib: CurveLibrary = serde_json::from_str(&content)?;
    Ok(lib)
}

/// 一時ファイルに書いてから置き換える（書いている途中で落ちても、前のライブラリが残る）
pub fn save_library_file(path: &Path, library: &CurveLibrary) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(library)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, content).with_context(|| format!("write library {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replace library {}", path.display()))?;
    Ok(())
}

/// 読めなかったライブラリを `{名前}.broken-{秒}` へ退避する。退避先を返す。
/// 退避しないまま保存すると、読めなかったファイルを今の中身で上書きしてしまう
pub fn backup_broken_file(path: &Path) -> std::io::Result<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".broken-{secs}"));
    let dest = PathBuf::from(name);
    fs::rename(path, &dest)?;
    Ok(dest)
}

pub fn merge_libraries(base: &mut CurveLibrary, other: &CurveLibrary) {
    for (id, doc) in &other.curves {
        base.curves.insert(*id, doc.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_json_roundtrip() {
        let mut lib = CurveLibrary::default();
        lib.insert(
            100,
            CurveDocument {
                mode: "spring".to_string(),
                script_code: "return t".to_string(),
                bezier: BezierDoc {
                    y0: 0.0,
                    x1: 0.1,
                    y1: 0.2,
                    x2: 0.3,
                    y2: 0.4,
                    y3: 1.0,
                },
                spring: SpringDoc {
                    zeta: 0.5,
                    omega: 12.0,
                },
                multi_anchors: vec![],
                composite_anchors: vec![],
                composite_segments: vec![],
                selected_segment: 0,
            },
        );
        let json = serde_json::to_string(&lib).unwrap();
        let restored: CurveLibrary = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.defined_count(), 1);
        assert_eq!(restored.get(100).unwrap().mode, "spring");
    }

    /// 項目が欠けた前の版のライブラリも読める（足りない項目は既定値）。知らない項目は無視する
    #[test]
    fn library_reads_older_and_newer_files() {
        let old = r#"{"curves":{"5":{"mode":"spring","script_code":"return t","bezier":{"x1":0.1,"y1":0.2,"x2":0.3,"y2":0.4},"spring":{"zeta":0.5,"omega":12.0}}}}"#;
        let lib: CurveLibrary = serde_json::from_str(old).unwrap();
        let d = lib.get(5).unwrap();
        assert_eq!(d.mode, "spring");
        assert!(d.multi_anchors.is_empty() && d.selected_segment == 0);
        assert_eq!(d.bezier.y3, 1.0);
        let newer = r#"{"curves":{"6":{"mode":"bezier","future_field":42}},"version":3}"#;
        let lib: CurveLibrary = serde_json::from_str(newer).unwrap();
        assert_eq!(lib.get(6).unwrap().mode, "bezier");
    }

    #[test]
    fn save_and_backup_files() {
        let dir = std::env::temp_dir().join(format!("ce2_library_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.CurveEditor2Library.json");
        let mut lib = CurveLibrary::default();
        lib.insert(3, CurveDocument::default());
        save_library_file(&path, &lib).unwrap();
        assert_eq!(load_library_file(&path).unwrap().defined_count(), 1);
        assert!(!dir.join("p.CurveEditor2Library.json.tmp").exists());
        fs::write(&path, "{ broken").unwrap();
        assert!(load_library_file(&path).is_err());
        let dest = backup_broken_file(&path).unwrap();
        assert!(!path.exists() && fs::read_to_string(&dest).unwrap() == "{ broken");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn library_id_bounds() {
        let mut lib = CurveLibrary::default();
        let doc = CurveDocument {
            mode: "bezier".to_string(),
            script_code: String::new(),
            bezier: BezierDoc {
                y0: 0.0,
                x1: 0.0,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0,
                y3: 1.0,
            },
            spring: SpringDoc {
                zeta: 0.4,
                omega: 10.0,
            },
            multi_anchors: vec![],
            composite_anchors: vec![],
            composite_segments: vec![],
            selected_segment: 0,
        };
        lib.insert(0, doc.clone());
        lib.insert(MAX_CURVE_ID + 1, doc.clone());
        lib.insert(42, doc);
        assert_eq!(lib.defined_count(), 1);
        assert!(lib.get(42).is_some());
    }
}
