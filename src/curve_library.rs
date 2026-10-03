//! カーブライブラリ（ID 1〜524288）のシリアライズと永続化

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const MAX_CURVE_ID: u32 = 524_288;
pub const PROJECT_SERDE_KEY: &str = "curveeditor2/library";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorDoc {
    pub t: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpringDoc {
    pub zeta: f64,
    pub omega: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

pub fn save_library_file(path: &Path, library: &CurveLibrary) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(library)?;
    fs::write(path, content).with_context(|| format!("write library {}", path.display()))?;
    Ok(())
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
