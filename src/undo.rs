//! エディタ内カーブ編集の Undo / Redo
//!
//! 編集箇所ごとに「変更前の状態を積む」呼び出しを置く方式（v0.7.6 まで）は、
//! 後から足した操作（反転・座標パネル・ダブルクリック追加）で登録漏れが出た。
//! ここでは「最後に確定した状態（baseline）」を 1 つ持ち、
//! フレームの終わりに現在のモデルと比べて、操作が終わっていれば 1 段として確定する。
//! どの経路でモデルが変わっても拾えるので、呼び出し側に登録の義務が無い。
//!
//! 表示（ズーム・パン）と選択は対象外。これらだけを変えた操作は Undo の段にしない。
//!
//! なお本体（AviUtl2）の Undo とは別物で、互いに干渉しない。本体の「元に戻す」が
//! 直前の操作を戻さない不具合は別プラグインが原因だった
//! （`AI/host/issues/20260913_host_undo_last_operation_lost.md`）。

use std::collections::VecDeque;

use crate::curves::{Anchor, BezierParams, CompositeCurve, CurveMode, SpringParams};

pub const MAX_UNDO_LEVELS: usize = 100;

/// Undo の対象になるカーブのモデル。
#[derive(Debug, Clone, PartialEq)]
pub struct CurveSnapshot {
    pub mode: CurveMode,
    pub script_code: String,
    pub bezier: BezierParams,
    pub spring: SpringParams,
    pub multi_anchors: Vec<Anchor>,
    pub composite: CompositeCurve,
}

#[derive(Debug, Default, Clone)]
pub struct UndoHistory {
    undo_stack: VecDeque<CurveSnapshot>,
    redo_stack: Vec<CurveSnapshot>,
    /// 最後に確定した（または復元した）状態。これとの差分を 1 段の編集とみなす。
    baseline: Option<CurveSnapshot>,
}

impl UndoHistory {
    /// 履歴を捨てて、現在の状態を起点にする（プロジェクト読込時など）。
    pub fn reset(&mut self, current: CurveSnapshot) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.baseline = Some(current);
    }

    pub fn baseline(&self) -> Option<&CurveSnapshot> {
        self.baseline.as_ref()
    }

    /// baseline からの変化を 1 段として確定する。確定したら `true`。
    /// 新しい編集を確定すると Redo 履歴は消える。
    pub fn commit(&mut self, current: CurveSnapshot) -> bool {
        match &self.baseline {
            None => {
                self.baseline = Some(current);
                false
            }
            Some(base) if *base == current => false,
            Some(_) => {
                if let Some(previous) = self.baseline.replace(current) {
                    self.push_undo(previous);
                }
                self.redo_stack.clear();
                true
            }
        }
    }

    /// 1 段戻す。戻り値は復元すべき状態。
    /// 未確定の変更がある場合は、呼び出し側が先に [`Self::commit`] しておくこと。
    pub fn undo(&mut self) -> Option<CurveSnapshot> {
        let previous = self.undo_stack.pop_back()?;
        if let Some(current) = self.baseline.replace(previous.clone()) {
            self.redo_stack.push(current);
        }
        Some(previous)
    }

    /// 1 段やり直す。戻り値は復元すべき状態。
    pub fn redo(&mut self) -> Option<CurveSnapshot> {
        let next = self.redo_stack.pop()?;
        if let Some(current) = self.baseline.replace(next.clone()) {
            self.push_undo(current);
        }
        Some(next)
    }

    pub fn has_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn has_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn push_undo(&mut self, snapshot: CurveSnapshot) {
        self.undo_stack.push_back(snapshot);
        while self.undo_stack.len() > MAX_UNDO_LEVELS {
            self.undo_stack.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(code: &str) -> CurveSnapshot {
        CurveSnapshot {
            mode: CurveMode::Script,
            script_code: code.to_string(),
            bezier: BezierParams::default(),
            spring: SpringParams::default(),
            multi_anchors: Vec::new(),
            composite: CompositeCurve::default(),
        }
    }

    #[test]
    fn first_commit_only_sets_baseline() {
        let mut h = UndoHistory::default();
        assert!(!h.commit(snap("a")));
        assert!(!h.has_undo());
        assert_eq!(h.baseline().unwrap().script_code, "a");
    }

    #[test]
    fn unchanged_state_is_not_a_step() {
        let mut h = UndoHistory::default();
        h.reset(snap("a"));
        assert!(!h.commit(snap("a")));
        assert!(!h.has_undo());
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut h = UndoHistory::default();
        h.reset(snap("a"));
        assert!(h.commit(snap("b")));
        assert!(h.commit(snap("c")));

        assert_eq!(h.undo().unwrap().script_code, "b");
        assert_eq!(h.undo().unwrap().script_code, "a");
        assert!(h.undo().is_none(), "起点より前には戻らない");

        assert_eq!(h.redo().unwrap().script_code, "b");
        assert_eq!(h.redo().unwrap().script_code, "c");
        assert!(h.redo().is_none());
        assert_eq!(h.baseline().unwrap().script_code, "c");
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let mut h = UndoHistory::default();
        h.reset(snap("a"));
        h.commit(snap("b"));
        h.undo();
        assert!(h.has_redo());
        assert!(h.commit(snap("x")));
        assert!(!h.has_redo(), "分岐した編集の後に古い Redo を残さない");
        assert_eq!(h.undo().unwrap().script_code, "a");
    }

    /// 未確定の変更は、先に commit してから undo すると「その変更」を戻す。
    #[test]
    fn uncommitted_change_is_undone_first() {
        let mut h = UndoHistory::default();
        h.reset(snap("a"));
        h.commit(snap("b"));
        // "c" を編集したが、まだ確定していない状態で Undo が押された
        h.commit(snap("c"));
        assert_eq!(h.undo().unwrap().script_code, "b");
    }

    #[test]
    fn history_is_capped() {
        let mut h = UndoHistory::default();
        h.reset(snap("0"));
        for i in 1..=(MAX_UNDO_LEVELS + 20) {
            h.commit(snap(&i.to_string()));
        }
        let mut count = 0;
        while h.undo().is_some() {
            count += 1;
        }
        assert_eq!(count, MAX_UNDO_LEVELS);
    }
}
