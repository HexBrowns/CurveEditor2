//! イージングカーブ計算と Lua コード生成

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveMode {
    Bezier,
    MultiBezier,
    Spring,
    Bounce,
    Composite,
    Script,
}

impl CurveMode {
    pub const ALL: [CurveMode; 6] = [
        CurveMode::Bezier,
        CurveMode::MultiBezier,
        CurveMode::Spring,
        CurveMode::Bounce,
        CurveMode::Composite,
        CurveMode::Script,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CurveMode::Bezier => "ベジェ",
            CurveMode::MultiBezier => "マルチベジェ",
            CurveMode::Spring => "振動",
            CurveMode::Bounce => "バウンス",
            CurveMode::Composite => "標準(複合)",
            CurveMode::Script => "スクリプト",
        }
    }

    /// 狭いウィンドウ向けの短い表示名。
    pub fn short_label(self) -> &'static str {
        match self {
            CurveMode::Bezier => "ベジェ",
            CurveMode::MultiBezier => "MB",
            CurveMode::Spring => "振動",
            CurveMode::Bounce => "Bn",
            CurveMode::Composite => "複合",
            CurveMode::Script => "Lua",
        }
    }

    /// UI 向けの一言説明（モード差の認知負荷を下げる）。
    pub fn hint(self) -> &'static str {
        match self {
            CurveMode::Bezier => "CSS 相当の2制御点ベジェ",
            CurveMode::MultiBezier => "複数アンカー。各区間は自動 ease-in-out（種類は固定）",
            CurveMode::Spring => "減衰振動（ζ / ω）。グラフ操作はプレビューのみ",
            CurveMode::Bounce => "バウンスアウト。グラフ操作はプレビューのみ",
            CurveMode::Composite => "区間ごとに直線/ベジェ/振動/バウンスを切り替え可能",
            CurveMode::Script => "Lua を直接編集（return t 形式）",
        }
    }
}

/// 標準(複合)モードの区間種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Linear,
    Bezier,
    Spring,
    Bounce,
}

impl SegmentKind {
    pub const ALL: [SegmentKind; 4] = [
        SegmentKind::Linear,
        SegmentKind::Bezier,
        SegmentKind::Spring,
        SegmentKind::Bounce,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SegmentKind::Linear => "直線",
            SegmentKind::Bezier => "ベジェ",
            SegmentKind::Spring => "振動",
            SegmentKind::Bounce => "バウンス",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            SegmentKind::Linear => "直",
            SegmentKind::Bezier => "ベ",
            SegmentKind::Spring => "振",
            SegmentKind::Bounce => "Bn",
        }
    }
}

/// 標準(複合)モードの1区間。区間の始点・終点は前後の `Anchor` を参照する。
#[derive(Debug, Clone, PartialEq)]
pub struct CompositeSegment {
    pub kind: SegmentKind,
    /// ベジェ区間用の制御点（区間内ローカル座標、0.0〜1.0を基本とするが超過も可）。
    pub cx1: f64,
    pub cy1: f64,
    pub cx2: f64,
    pub cy2: f64,
    /// 振動区間用パラメータ。
    pub zeta: f64,
    pub omega: f64,
}

impl Default for CompositeSegment {
    fn default() -> Self {
        Self {
            kind: SegmentKind::Bezier,
            cx1: 0.33,
            cy1: 0.0,
            cx2: 0.67,
            cy2: 1.0,
            zeta: 0.4,
            omega: 10.0,
        }
    }
}

impl CompositeSegment {
    pub fn eval(&self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        match self.kind {
            SegmentKind::Linear => u,
            SegmentKind::Bezier => eval_bezier(
                u,
                &BezierParams {
                    y0: 0.0,
                    x1: self.cx1,
                    y1: self.cy1,
                    x2: self.cx2,
                    y2: self.cy2,
                    y3: 1.0,
                },
            ),
            SegmentKind::Spring => eval_spring(
                u,
                &SpringParams {
                    zeta: self.zeta,
                    omega: self.omega,
                },
            ),
            SegmentKind::Bounce => bounce_out(u),
        }
    }
}

/// 標準(複合)モードの全体データ: アンカー列(n+1) と区間列(n)。
#[derive(Debug, Clone, PartialEq)]
pub struct CompositeCurve {
    pub anchors: Vec<Anchor>,
    pub segments: Vec<CompositeSegment>,
}

impl Default for CompositeCurve {
    fn default() -> Self {
        Self {
            anchors: vec![Anchor::new(0.0, 0.0), Anchor::new(1.0, 1.0)],
            segments: vec![CompositeSegment::default()],
        }
    }
}

impl CompositeCurve {
    pub fn segment_index_for(&self, t: f64) -> usize {
        let t = t.clamp(0.0, 1.0);
        for i in 0..self.segments.len() {
            let a0 = self.anchors[i];
            let a1 = self.anchors[i + 1];
            if t <= a1.t + 1e-9 || i == self.segments.len() - 1 {
                if t >= a0.t - 1e-9 {
                    return i;
                }
            }
        }
        self.segments.len().saturating_sub(1)
    }

    pub fn eval(&self, t: f64) -> f64 {
        if self.anchors.len() < 2 || self.segments.is_empty() {
            return t.clamp(0.0, 1.0);
        }
        let t = t.clamp(0.0, 1.0);
        let i = self.segment_index_for(t);
        let a0 = self.anchors[i];
        let a1 = self.anchors[i + 1];
        let dt = a1.t - a0.t;
        if dt < 1e-9 {
            return a0.y;
        }
        let u = ((t - a0.t) / dt).clamp(0.0, 1.0);
        let factor = self.segments[i].eval(u);
        a0.y + (a1.y - a0.y) * factor
    }

    pub fn insert_anchor_at(&mut self, t: f64, y: f64) {
        let i = self.segment_index_for(t);
        let new_anchor = Anchor::new(t, y);
        self.anchors.insert(i + 1, new_anchor);
        let new_segment = CompositeSegment {
            kind: self.segments[i].kind,
            ..CompositeSegment::default()
        };
        self.segments.insert(i + 1, new_segment);
    }

    pub fn remove_anchor(&mut self, idx: usize) {
        if idx == 0 || idx + 1 >= self.anchors.len() || self.anchors.len() <= 2 {
            return;
        }
        self.anchors.remove(idx);
        // 前後どちらかの区間を消す。前区間側に統合する。
        if idx > 0 && idx - 1 < self.segments.len() {
            self.segments.remove(idx);
        } else if !self.segments.is_empty() {
            self.segments.remove(0);
        }
    }

    /// アンカーを隣接アンカーの間にクランプする（並び替えは行わない）。
    /// ドラッグ中に呼び出すことで、区間と `segments` の対応関係を崩さずに済む。
    pub fn clamp_anchor(&mut self, idx: usize) {
        if idx == 0 || idx + 1 >= self.anchors.len() {
            if idx == 0 {
                self.anchors[0].t = 0.0;
            }
            if idx + 1 == self.anchors.len() {
                if let Some(last) = self.anchors.last_mut() {
                    last.t = 1.0;
                }
            }
            return;
        }
        let lo = self.anchors[idx - 1].t + 0.01;
        let hi = self.anchors[idx + 1].t - 0.01;
        self.anchors[idx].t = self.anchors[idx].t.clamp(lo.min(hi), hi.max(lo));
    }
}

/// 壊れた複合カーブを最低限の形に直す（Lua 手入力・ライブラリ読込の受け口）。
pub fn normalize_composite(composite: &mut CompositeCurve, selected_segment: &mut usize) {
    if composite.anchors.len() < 2 {
        *composite = CompositeCurve::default();
    }
    if composite.segments.is_empty() {
        composite.segments.push(CompositeSegment::default());
    }
    *selected_segment = (*selected_segment).min(composite.segments.len().saturating_sub(1));
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub t: f64,
    pub y: f64,
}

impl Anchor {
    pub fn new(t: f64, y: f64) -> Self {
        Self { t, y }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BezierParams {
    /// 始点の y（t=0）。既定 0。
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    /// 終点の y（t=1）。既定 1。
    pub y3: f64,
}

impl Default for BezierParams {
    fn default() -> Self {
        Self {
            y0: 0.0,
            x1: 0.42,
            y1: 0.0,
            x2: 0.58,
            y2: 1.0,
            y3: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpringParams {
    pub zeta: f64,
    pub omega: f64,
}

impl Default for SpringParams {
    fn default() -> Self {
        Self {
            zeta: 0.4,
            omega: 10.0,
        }
    }
}

pub fn default_multi_anchors() -> Vec<Anchor> {
    vec![Anchor::new(0.0, 0.0), Anchor::new(1.0, 1.0)]
}

const PI: f64 = std::f64::consts::PI;

fn bounce_out(t: f64) -> f64 {
    if t < 1.0 / 2.75 {
        7.5625 * t * t
    } else if t < 2.0 / 2.75 {
        let t = t - 1.5 / 2.75;
        7.5625 * t * t + 0.75
    } else if t < 2.5 / 2.75 {
        let t = t - 2.25 / 2.75;
        7.5625 * t * t + 0.9375
    } else {
        let t = t - 2.625 / 2.75;
        7.5625 * t * t + 0.984375
    }
}

pub fn eval_bezier(t: f64, p: &BezierParams) -> f64 {
    let bx = |s: f64| {
        3.0 * s * (1.0 - s).powi(2) * p.x1 + 3.0 * s.powi(2) * (1.0 - s) * p.x2 + s.powi(3)
    };
    let by = |s: f64| {
        let u = 1.0 - s;
        u * u * u * p.y0
            + 3.0 * u * u * s * p.y1
            + 3.0 * u * s * s * p.y2
            + s * s * s * p.y3
    };
    let mut lo = 0.0;
    let mut hi = 1.0;
    let mut s = t.clamp(0.0, 1.0);
    for _ in 0..25 {
        let x = bx(s);
        if (x - t).abs() < 1e-7 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) * 0.5;
    }
    by(s)
}

/// マルチアンカー: 各セグメントは端点間を ease-in-out ベジェで補間
pub fn eval_multi_bezier(t: f64, anchors: &[Anchor]) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if anchors.len() < 2 {
        return t;
    }
    let ease = BezierParams {
        y0: 0.0,
        x1: 0.33,
        y1: 0.0,
        x2: 0.67,
        y2: 1.0,
        y3: 1.0,
    };
    for i in 0..anchors.len() - 1 {
        let a0 = anchors[i];
        let a1 = anchors[i + 1];
        if t <= a1.t + 1e-9 || i == anchors.len() - 2 {
            if t >= a0.t - 1e-9 {
                let dt = a1.t - a0.t;
                if dt < 1e-9 {
                    return a0.y;
                }
                let u = ((t - a0.t) / dt).clamp(0.0, 1.0);
                let factor = eval_bezier(u, &ease);
                return a0.y + (a1.y - a0.y) * factor;
            }
        }
    }
    anchors.last().map(|a| a.y).unwrap_or(t)
}

pub fn sort_anchors(anchors: &mut Vec<Anchor>) {
    anchors.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(first) = anchors.first_mut() {
        first.t = 0.0;
    }
    if let Some(last) = anchors.last_mut() {
        last.t = 1.0;
    }
}

/// カーブの反転方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlipAxis {
    /// 左右反転（時間反転）: `y'(t) = y(1-t)`。始点と終点の値が入れ替わる。
    Horizontal,
    /// 上下反転（値反転）: 端点 y の中点を軸に折り返す。こちらも端点が入れ替わる。
    Vertical,
}

/// そのモードが反転操作を持つか。
///
/// 振動・バウンス・スクリプトは形状をパラメータで折り返せない（振動は減衰の向き、
/// バウンスは固定式、スクリプトは任意の Lua）ため対象外。
pub fn mode_supports_flip(mode: CurveMode) -> bool {
    matches!(
        mode,
        CurveMode::Bezier | CurveMode::MultiBezier | CurveMode::Composite
    )
}

/// 2制御点ベジェを反転する。
///
/// 左右反転は制御点列を逆順にする（P0..P3 → P3..P0、x は 1-x）。
/// 上下反転は端点 y の中点 m を軸に `y' = 2m - y`。
/// 両方を続けて適用すると 180° 回転になり、端点を保ったまま
/// ease-in ⇄ ease-out が入れ替わる。
pub fn flip_bezier(p: &mut BezierParams, axis: FlipAxis) {
    match axis {
        FlipAxis::Horizontal => {
            let old = p.clone();
            p.y0 = old.y3;
            p.x1 = 1.0 - old.x2;
            p.y1 = old.y2;
            p.x2 = 1.0 - old.x1;
            p.y2 = old.y1;
            p.y3 = old.y0;
        }
        FlipAxis::Vertical => {
            // 2m = y0 + y3。先に合計を取ってから各 y を折り返す。
            let sum = p.y0 + p.y3;
            p.y0 = sum - p.y0;
            p.y1 = sum - p.y1;
            p.y2 = sum - p.y2;
            p.y3 = sum - p.y3;
        }
    }
}

/// マルチベジェのアンカー列を反転する。
///
/// 区間の補間は固定 ease-in-out (0.33,0,0.67,1) で 180° 回転に対して対称なので、
/// アンカーを写すだけで厳密な鏡像になる。
pub fn flip_anchors(anchors: &mut Vec<Anchor>, axis: FlipAxis) {
    if anchors.len() < 2 {
        return;
    }
    match axis {
        FlipAxis::Horizontal => {
            anchors.reverse();
            for a in anchors.iter_mut() {
                a.t = 1.0 - a.t;
            }
            // 端点 t を 0/1 に正規化する（1-t の丸め誤差を残さない）。
            sort_anchors(anchors);
        }
        FlipAxis::Vertical => {
            let sum = anchors[0].y + anchors[anchors.len() - 1].y;
            for a in anchors.iter_mut() {
                a.y = sum - a.y;
            }
        }
    }
}

/// 標準(複合)カーブを反転し、形を写せなかった区間の数を返す。
///
/// 上下反転はアンカーだけで足りる。区間は世界座標で
/// `y = a0 + (a1-a0)·f(u)` なので、両端を折り返せば `y' = 2m - y` が
/// そのまま成り立ち、`f` は触らなくてよい（種別を問わず厳密）。
///
/// 左右反転はアンカーと区間の並びを逆にしたうえで、各区間の `f` を
/// `f'(u) = 1 - f(1-u)` に置き換える必要がある。直線とベジェは制御点の
/// 付け替えで表現できるが、振動とバウンスは表現できないので形は元のまま残り、
/// その件数を返す（呼び出し側でユーザーに伝える）。
pub fn flip_composite(c: &mut CompositeCurve, axis: FlipAxis) -> usize {
    if c.anchors.len() < 2 || c.segments.is_empty() {
        return 0;
    }
    match axis {
        FlipAxis::Horizontal => {
            c.anchors.reverse();
            for a in c.anchors.iter_mut() {
                a.t = 1.0 - a.t;
            }
            if let Some(first) = c.anchors.first_mut() {
                first.t = 0.0;
            }
            if let Some(last) = c.anchors.last_mut() {
                last.t = 1.0;
            }
            c.segments.reverse();
            let mut unflippable = 0;
            for seg in c.segments.iter_mut() {
                match seg.kind {
                    SegmentKind::Bezier => {
                        let (cx1, cy1, cx2, cy2) = (seg.cx1, seg.cy1, seg.cx2, seg.cy2);
                        seg.cx1 = 1.0 - cx2;
                        seg.cy1 = 1.0 - cy2;
                        seg.cx2 = 1.0 - cx1;
                        seg.cy2 = 1.0 - cy1;
                    }
                    SegmentKind::Linear => {}
                    SegmentKind::Spring | SegmentKind::Bounce => unflippable += 1,
                }
            }
            unflippable
        }
        FlipAxis::Vertical => {
            let sum = c.anchors[0].y + c.anchors[c.anchors.len() - 1].y;
            for a in c.anchors.iter_mut() {
                a.y = sum - a.y;
            }
            0
        }
    }
}

pub fn eval_spring(t: f64, p: &SpringParams) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let zeta = p.zeta.max(0.0);
    let omega = p.omega;
    if omega <= 0.0 {
        return t;
    }

    let tp = if zeta < 1e-6 {
        t * (2.0 * PI / omega)
    } else {
        t * (4.605 / (zeta * omega))
    };

    if zeta < 1.0 - 1e-6 {
        let wd = omega * (1.0 - zeta * zeta).sqrt();
        let env = (-zeta * omega * tp).exp();
        1.0 - env * (wd * tp).cos()
            - env * (zeta / (1.0 - zeta * zeta).sqrt()) * (wd * tp).sin()
    } else if zeta < 1.0 + 1e-6 {
        let env = (-omega * tp).exp();
        1.0 - env * (1.0 + omega * tp)
    } else {
        let s = (zeta * zeta - 1.0).sqrt();
        let r1 = -omega * (zeta - s);
        let r2 = -omega * (zeta + s);
        let a = r2 / (r1 - r2);
        let b = -r1 / (r1 - r2);
        1.0 + a * (r1 * tp).exp() + b * (r2 * tp).exp()
    }
}

pub fn eval_curve(
    mode: CurveMode,
    t: f64,
    bezier: &BezierParams,
    spring: &SpringParams,
    multi: &[Anchor],
    composite: &CompositeCurve,
) -> f64 {
    match mode {
        CurveMode::Bezier => eval_bezier(t, bezier),
        CurveMode::MultiBezier => eval_multi_bezier(t, multi),
        CurveMode::Spring => eval_spring(t, spring),
        CurveMode::Bounce => bounce_out(t.clamp(0.0, 1.0)),
        CurveMode::Composite => composite.eval(t),
        CurveMode::Script => t,
    }
}

pub fn generate_lua(
    mode: CurveMode,
    bezier: &BezierParams,
    spring: &SpringParams,
    multi: &[Anchor],
    composite: &CompositeCurve,
    script: &str,
) -> String {
    match mode {
        CurveMode::Bezier => generate_bezier_lua(bezier),
        CurveMode::MultiBezier => generate_multi_lua(multi),
        CurveMode::Spring => format!(
            "-- CurveEditor_H: 振動 (spring)\nreturn spring(t, {:.4}, {:.4})",
            spring.zeta, spring.omega
        ),
        CurveMode::Bounce => "-- CurveEditor_H: バウンス\nreturn bounce(t)".to_string(),
        CurveMode::Composite => generate_composite_lua(composite),
        CurveMode::Script => {
            let trimmed = script.trim();
            if trimmed.is_empty() {
                "return t".to_string()
            } else {
                script.to_string()
            }
        }
    }
}

fn generate_bezier_lua(bezier: &BezierParams) -> String {
    let y0_default = (bezier.y0 - 0.0).abs() < 1e-6;
    let y3_default = (bezier.y3 - 1.0).abs() < 1e-6;
    // 端点が CSS cubic-bezier 既定 (0→1) のときだけ組み込み bezier() を使う。
    // y0==y3 でも制御点で弧を描けるため、dy≈0 で定値に短絡してはならない。
    if y0_default && y3_default {
        return format!(
            "-- CurveEditor_H: ベジェ\nreturn bezier(t, {:.4}, {:.4}, {:.4}, {:.4})",
            bezier.x1, bezier.y1, bezier.x2, bezier.y2
        );
    }
    // 絶対座標の三次ベジェ（X を二分木探索 → Y）。eval_bezier と同型。
    format!(
        r#"-- CurveEditor_H: ベジェ
local x1, y1, x2, y2 = {x1:.4}, {y1:.4}, {x2:.4}, {y2:.4}
local y0, y3 = {y0:.4}, {y3:.4}
if t < 0 then t = 0 elseif t > 1 then t = 1 end
local function bx(s)
  local u = 1 - s
  return 3 * s * u * u * x1 + 3 * s * s * u * x2 + s * s * s
end
local function by(s)
  local u = 1 - s
  return u * u * u * y0 + 3 * u * u * s * y1 + 3 * u * s * s * y2 + s * s * s * y3
end
local lo, hi, s = 0, 1, t
for i = 1, 25 do
  local x = bx(s)
  if math.abs(x - t) < 1e-7 then break end
  if x < t then lo = s else hi = s end
  s = (lo + hi) * 0.5
end
return by(s)"#,
        x1 = bezier.x1,
        y1 = bezier.y1,
        x2 = bezier.x2,
        y2 = bezier.y2,
        y0 = bezier.y0,
        y3 = bezier.y3,
    )
}

fn generate_composite_lua(c: &CompositeCurve) -> String {
    if c.anchors.len() < 2 || c.segments.is_empty() {
        return "return t".to_string();
    }
    let mut lines = vec![
        "-- CurveEditor_H: 標準(複合)".to_string(),
        "local anchors = {".to_string(),
    ];
    for a in &c.anchors {
        lines.push(format!("  {{ {:.6}, {:.6} }},", a.t, a.y));
    }
    lines.push("}".to_string());
    lines.push("local segs = {".to_string());
    for s in &c.segments {
        let kind_str = match s.kind {
            SegmentKind::Linear => "linear",
            SegmentKind::Bezier => "bezier",
            SegmentKind::Spring => "spring",
            SegmentKind::Bounce => "bounce",
        };
        lines.push(format!(
            "  {{ \"{}\", {:.6}, {:.6}, {:.6}, {:.6}, {:.6}, {:.6} }},",
            kind_str, s.cx1, s.cy1, s.cx2, s.cy2, s.zeta, s.omega
        ));
    }
    lines.push("}".to_string());
    lines.push(
        r#"local function seg_eval(seg, u)
  local kind = seg[1]
  if kind == "linear" then return u
  elseif kind == "bezier" then return bezier(u, seg[2], seg[3], seg[4], seg[5])
  elseif kind == "spring" then return spring(u, seg[6], seg[7])
  elseif kind == "bounce" then return bounce(u)
  end
  return u
end
"#
        .to_string(),
    );
    lines.push("for i = 1, #segs do".to_string());
    lines.push("  local a0, a1 = anchors[i], anchors[i + 1]".to_string());
    lines.push("  if (t >= a0[1] and t <= a1[1]) or i == #segs then".to_string());
    lines.push("    local u = (a1[1] - a0[1] > 1e-9) and (t - a0[1]) / (a1[1] - a0[1]) or 0".to_string());
    lines.push("    u = clamp(u, 0, 1)".to_string());
    lines.push("    local factor = seg_eval(segs[i], u)".to_string());
    lines.push("    return a0[2] + (a1[2] - a0[2]) * factor".to_string());
    lines.push("  end".to_string());
    lines.push("end".to_string());
    lines.push("return anchors[#anchors][2]".to_string());
    lines.join("\n")
}

fn generate_multi_lua(anchors: &[Anchor]) -> String {
    if anchors.len() < 2 {
        return "return t".to_string();
    }
    let mut lines = vec![
        "-- CurveEditor_H: マルチベジェ".to_string(),
        "local anchors = {".to_string(),
    ];
    for a in anchors {
        lines.push(format!("  {{ {:.6}, {:.6} }},", a.t, a.y));
    }
    lines.push("}".to_string());
    lines.push(
        r#"local function seg_y(u, y0, y1)
  local dy = y1 - y0
  if math.abs(dy) < 1e-9 then return y0 end
  local y1c = y0 + dy / 3
  local y2c = y1 - dy / 3
  local eased = bezier(u, 1/3, (y1c - y0) / dy, 2/3, (y2c - y0) / dy)
  return y0 + dy * eased
end
"#
        .to_string(),
    );
    lines.push("for i = 1, #anchors - 1 do".to_string());
    lines.push("  local a0, a1 = anchors[i], anchors[i + 1]".to_string());
    lines.push("  if t >= a0[1] and t <= a1[1] then".to_string());
    lines.push("    local u = (t - a0[1]) / (a1[1] - a0[1])".to_string());
    lines.push("    return seg_y(u, a0[2], a1[2])".to_string());
    lines.push("  end".to_string());
    lines.push("end".to_string());
    lines.push("return anchors[#anchors][2]".to_string());
    lines.join("\n")
}

pub fn sample_curve(
    mode: CurveMode,
    bezier: &BezierParams,
    spring: &SpringParams,
    multi: &[Anchor],
    composite: &CompositeCurve,
    steps: usize,
) -> Vec<[f64; 2]> {
    (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            [t, eval_curve(mode, t, bezier, spring, multi, composite)]
        })
        .collect()
}

/// スクリプトモード用の近似プレビュー（既知の生成形式のみ）。
#[derive(Debug, Clone)]
pub enum ScriptPreview {
    Bezier(BezierParams),
    Spring(SpringParams),
    Bounce,
    Multi(Vec<Anchor>),
    Composite(CompositeCurve),
}

impl ScriptPreview {
    pub fn label(&self) -> &'static str {
        match self {
            ScriptPreview::Bezier(_) => "bezier",
            ScriptPreview::Spring(_) => "spring",
            ScriptPreview::Bounce => "bounce",
            ScriptPreview::Multi(_) => "multi",
            ScriptPreview::Composite(_) => "composite",
        }
    }

    pub fn eval(&self, t: f64) -> f64 {
        match self {
            ScriptPreview::Bezier(p) => eval_bezier(t, p),
            ScriptPreview::Spring(p) => eval_spring(t, p),
            ScriptPreview::Bounce => bounce_out(t.clamp(0.0, 1.0)),
            ScriptPreview::Multi(a) => eval_multi_bezier(t, a),
            ScriptPreview::Composite(c) => c.eval(t),
        }
    }
}

/// 既知の CurveEditor_H 生成形式を検出し、近似プレビュー用データを返す。
pub fn detect_script_preview(code: &str) -> Option<ScriptPreview> {
    // anchors+segs を持つ複合を先に（本文中の bounce(u) 等で誤検出しない）
    if code.contains("local segs") || code.contains("segs =") {
        if let Ok(c) = parse_composite_from_lua(code) {
            return Some(ScriptPreview::Composite(c));
        }
    }
    if code.contains("local anchors") || code.contains("anchors =") {
        if let Ok(a) = parse_multi_anchors_from_lua(code) {
            return Some(ScriptPreview::Multi(a));
        }
    }
    if let Ok(p) = parse_bezier_from_lua(code) {
        return Some(ScriptPreview::Bezier(p));
    }
    if let Ok(p) = parse_spring_from_lua(code) {
        return Some(ScriptPreview::Spring(p));
    }
    if parse_bounce_from_lua(code).is_ok() {
        return Some(ScriptPreview::Bounce);
    }
    None
}

/// スクリプト／通常モード共通のサンプル列。Script 時は既知形式なら近似、さもなくば y=t。
pub fn sample_curve_with_script(
    mode: CurveMode,
    bezier: &BezierParams,
    spring: &SpringParams,
    multi: &[Anchor],
    composite: &CompositeCurve,
    script: &str,
    steps: usize,
) -> (Vec<[f64; 2]>, Option<&'static str>) {
    if mode == CurveMode::Script {
        if let Some(preview) = detect_script_preview(script) {
            let label = preview.label();
            let points = (0..=steps)
                .map(|i| {
                    let t = i as f64 / steps as f64;
                    [t, preview.eval(t)]
                })
                .collect();
            return (points, Some(label));
        }
        let points = (0..=steps)
            .map(|i| {
                let t = i as f64 / steps as f64;
                [t, t]
            })
            .collect();
        return (points, None);
    }
    (
        sample_curve(mode, bezier, spring, multi, composite, steps),
        None,
    )
}

/// 生成 Lua からベジェパラメータを読み取る（単純 / 絶対座標 / 旧スケール形式）。
pub fn parse_bezier_from_lua(code: &str) -> Result<BezierParams, String> {
    let (y0, y3) = parse_y0_y3_locals(code).unwrap_or((0.0, 1.0));

    // 絶対座標形式: local x1, y1, x2, y2 = ...
    if let Some(nums) = parse_xy_locals(code) {
        return Ok(BezierParams {
            y0,
            x1: nums[0],
            y1: nums[1],
            x2: nums[2],
            y2: nums[3],
            y3,
        });
    }

    let nums = parse_return_call_args(code, "bezier", 4)?;
    // 旧スケール形式: y0 + dy * bezier(t, x1, ny1, x2, ny2)
    let scaled = code.contains("local dy") || code.contains("dy * bezier");
    if scaled {
        let dy = y3 - y0;
        Ok(BezierParams {
            y0,
            x1: nums[0],
            y1: y0 + nums[1] * dy,
            x2: nums[2],
            y2: y0 + nums[3] * dy,
            y3,
        })
    } else {
        Ok(BezierParams {
            y0,
            x1: nums[0],
            y1: nums[1],
            x2: nums[2],
            y2: nums[3],
            y3,
        })
    }
}

fn parse_y0_y3_locals(code: &str) -> Option<(f64, f64)> {
    // local y0, y3 = 0.2, 0.8
    let key = "local y0";
    let start = code.find(key)?;
    let after = &code[start..];
    let eq = after.find('=')?;
    let rhs = after[eq + 1..].trim_start();
    let end = rhs.find('\n').unwrap_or(rhs.len());
    let line = rhs[..end].trim();
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let y0: f64 = parts[0].parse().ok()?;
    let y3: f64 = parts[1].parse().ok()?;
    Some((y0, y3))
}

fn parse_xy_locals(code: &str) -> Option<[f64; 4]> {
    // local x1, y1, x2, y2 = a, b, c, d
    let key = "local x1";
    let start = code.find(key)?;
    let after = &code[start..];
    let eq = after.find('=')?;
    let rhs = after[eq + 1..].trim_start();
    let end = rhs.find('\n').unwrap_or(rhs.len());
    let line = rhs[..end].trim();
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 4 {
        return None;
    }
    Some([
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
        parts[3].parse().ok()?,
    ])
}

/// 複合ベジェ制御点のワールド座標（dy≈0 のときは cy を絶対オフセットとして扱う）。
pub fn composite_cp_world(a0: Anchor, a1: Anchor, cx: f64, cy: f64) -> (f64, f64) {
    let dt = a1.t - a0.t;
    let dy = a1.y - a0.y;
    let t = a0.t + dt * cx;
    let y = if dy.abs() > 1e-9 {
        a0.y + dy * cy
    } else {
        a0.y + cy
    };
    (t, y)
}

/// ワールド座標から複合ベジェ制御点パラメータへ。
pub fn composite_cp_from_world(a0: Anchor, a1: Anchor, t: f64, y: f64) -> (f64, f64) {
    let dt = a1.t - a0.t;
    let dy = a1.y - a0.y;
    let cx = if dt.abs() > 1e-9 {
        (t - a0.t) / dt
    } else {
        0.0
    };
    let cy = if dy.abs() > 1e-9 {
        (y - a0.y) / dy
    } else {
        y - a0.y
    };
    (cx, cy)
}

/// `return spring(t, zeta, omega)` を読み取る。
pub fn parse_spring_from_lua(code: &str) -> Result<SpringParams, String> {
    let nums = parse_return_call_args(code, "spring", 2)?;
    Ok(SpringParams {
        zeta: nums[0],
        omega: nums[1],
    })
}

/// `return bounce(t)` を検出する（パラメータなし）。
pub fn parse_bounce_from_lua(code: &str) -> Result<(), String> {
    parse_return_call_args(code, "bounce", 0).map(|_| ())
}

/// トップレベルの `return fn_name(t, ...)` の数値引数を返す（先頭の `t` は除外）。
fn parse_return_call_args(
    code: &str,
    fn_name: &str,
    expected_nums: usize,
) -> Result<Vec<f64>, String> {
    // `return bezier(` / `return spring(` / `return bounce(` を優先（関数内の同名呼び出しを避ける）
    let needle = format!("return {fn_name}(");
    let start = code
        .find(&needle)
        .ok_or_else(|| format!("`return {fn_name}(...)` が見つかりません"))?;
    let after = &code[start + needle.len()..];
    let (inner, _) = read_balanced_paren_content(after)
        .ok_or_else(|| format!("`{fn_name}(...)` の括弧が閉じられていません"))?;

    let parts: Vec<&str> = split_top_level_commas(inner);
    if parts.is_empty() {
        return Err(format!("`{fn_name}(...)` の引数が空です"));
    }
    let num_parts = if expected_nums == 0 {
        Vec::new()
    } else {
        parts[1..].to_vec()
    };
    if num_parts.len() != expected_nums {
        return Err(format!(
            "`{fn_name}` の数値引数が {expected_nums} 個必要です (現在: {})",
            num_parts.len()
        ));
    }
    let mut nums = Vec::with_capacity(expected_nums);
    for p in num_parts {
        let n: f64 = p
            .trim()
            .parse()
            .map_err(|_| format!("数値の解析に失敗: {}", p.trim()))?;
        nums.push(n);
    }
    Ok(nums)
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'{' | b'[' => depth += 1,
            b')' | b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start <= s.len() {
        let rest = s[start..].trim();
        if !rest.is_empty() || !parts.is_empty() {
            parts.push(&s[start..]);
        }
    }
    parts
}

fn read_balanced_paren_content(s: &str) -> Option<(&str, usize)> {
    read_balanced_brace_content(s, '(', ')')
}

/// 生成 Lua から `local anchors = { ... }` を読み取り、マルチベジェ用アンカー列に変換する。
pub fn parse_multi_anchors_from_lua(code: &str) -> Result<Vec<Anchor>, String> {
    let pairs = parse_named_lua_pair_table(code, "anchors")?;
    validate_anchor_pairs(&pairs)?;
    Ok(pairs
        .into_iter()
        .map(|(t, y)| Anchor::new(t, y))
        .collect())
}

/// 生成 Lua から `anchors` と `segs` を読み取り、複合カーブに変換する。
pub fn parse_composite_from_lua(code: &str) -> Result<CompositeCurve, String> {
    let anchor_pairs = parse_named_lua_pair_table(code, "anchors")?;
    validate_anchor_pairs(&anchor_pairs)?;
    let anchors: Vec<Anchor> = anchor_pairs
        .into_iter()
        .map(|(t, y)| Anchor::new(t, y))
        .collect();

    let seg_rows = parse_named_lua_seg_table(code, "segs")?;
    if anchors.len() != seg_rows.len() + 1 {
        return Err(format!(
            "anchors と segs の件数が一致しません (anchors={}, segs={})",
            anchors.len(),
            seg_rows.len()
        ));
    }

    let segments = seg_rows
        .into_iter()
        .map(|row| {
            Ok(CompositeSegment {
                kind: parse_segment_kind(&row.kind)?,
                cx1: row.nums[0],
                cy1: row.nums[1],
                cx2: row.nums[2],
                cy2: row.nums[3],
                zeta: row.nums[4],
                omega: row.nums[5],
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(CompositeCurve {
        anchors,
        segments,
    })
}

fn validate_anchor_pairs(pairs: &[(f64, f64)]) -> Result<(), String> {
    if pairs.len() < 2 {
        return Err("アンカーは最低2点必要です".into());
    }
    if (pairs[0].0 - 0.0).abs() > 1e-3 {
        return Err(format!(
            "先頭アンカーの t は 0.0 である必要があります (現在: {:.6})",
            pairs[0].0
        ));
    }
    if (pairs.last().unwrap().0 - 1.0).abs() > 1e-3 {
        return Err(format!(
            "末尾アンカーの t は 1.0 である必要があります (現在: {:.6})",
            pairs.last().unwrap().0
        ));
    }
    for w in pairs.windows(2) {
        if w[1].0 <= w[0].0 + 1e-9 {
            return Err("アンカーの t は単調増加である必要があります".into());
        }
    }
    Ok(())
}

fn parse_segment_kind(s: &str) -> Result<SegmentKind, String> {
    match s {
        "linear" => Ok(SegmentKind::Linear),
        "bezier" => Ok(SegmentKind::Bezier),
        "spring" => Ok(SegmentKind::Spring),
        "bounce" => Ok(SegmentKind::Bounce),
        other => Err(format!("不明な区間種別: {other}")),
    }
}

struct SegRow {
    kind: String,
    nums: [f64; 6],
}

fn parse_named_lua_pair_table(code: &str, name: &str) -> Result<Vec<(f64, f64)>, String> {
    let body = extract_named_table_body(code, name)
        .ok_or_else(|| format!("`{name}` テーブルが見つかりません"))?;
    parse_lua_pair_rows(&body)
}

fn parse_named_lua_seg_table(code: &str, name: &str) -> Result<Vec<SegRow>, String> {
    let body = extract_named_table_body(code, name)
        .ok_or_else(|| format!("`{name}` テーブルが見つかりません"))?;
    parse_lua_seg_rows(&body)
}

/// `local name = { ... }` の `{ ... }` 内テキストを返す。
fn extract_named_table_body<'a>(code: &'a str, name: &str) -> Option<&'a str> {
    let local_pat = format!("local {name}");
    let start = code
        .find(&local_pat)
        .or_else(|| code.find(name))?;
    let after = &code[start..];
    let eq = after.find('=')?;
    let mut rest = after[eq + 1..].trim_start();
    if !rest.starts_with('{') {
        return None;
    }
    rest = &rest[1..];
    let (body, _) = read_balanced_brace_content(rest, '{', '}')?;
    Some(body)
}

/// 外側の `{` の直後から始まり、対応する `}` の手前までのスライスを返す。
fn read_balanced_brace_content<'a>(
    s: &'a str,
    open: char,
    close: char,
) -> Option<(&'a str, usize)> {
    let mut depth = 1i32;
    let mut in_string = false;
    let mut quote = '"';
    let mut escape = false;
    let mut end_idx = None;

    for (i, ch) in s.char_indices() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == quote {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' | '\'' => {
                in_string = true;
                quote = ch;
            }
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    end_idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let end = end_idx?;
    Some((&s[..end], end + 1))
}

fn parse_lua_pair_rows(body: &str) -> Result<Vec<(f64, f64)>, String> {
    let mut pairs = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] != b'{' {
            return Err(format!(
                "アンカー行の解析に失敗しました (位置 {i} 付近)"
            ));
        }
        i += 1;
        let (inner, consumed) = read_balanced_brace_content(&body[i..], '{', '}')
            .ok_or_else(|| "アンカー行の括弧が閉じられていません".to_string())?;
        let nums = split_lua_numbers(inner.trim())?;
        if nums.len() < 2 {
            return Err("アンカー行には t, y の2値が必要です".into());
        }
        pairs.push((nums[0], nums[1]));
        i += consumed;
    }
    Ok(pairs)
}

fn parse_lua_seg_rows(body: &str) -> Result<Vec<SegRow>, String> {
    let mut rows = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] != b'{' {
            return Err(format!("区間行の解析に失敗しました (位置 {i} 付近)"));
        }
        i += 1;
        let (inner, consumed) = read_balanced_brace_content(&body[i..], '{', '}')
            .ok_or_else(|| "区間行の括弧が閉じられていません".to_string())?;
        let (kind, nums) = split_lua_seg_fields(inner.trim())?;
        if nums.len() < 6 {
            return Err(format!(
                "区間行には種別 + 6 数値が必要です (kind={kind})"
            ));
        }
        rows.push(SegRow {
            kind,
            nums: [nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]],
        });
        i += consumed;
    }
    Ok(rows)
}

fn split_lua_seg_fields(inner: &str) -> Result<(String, Vec<f64>), String> {
    let mut kind = None;
    let mut nums = Vec::new();
    let mut in_string = false;
    let mut quote = '"';
    let mut escape = false;
    let mut token = String::new();
    let mut tokens: Vec<String> = Vec::new();

    let flush = |token: &mut String, tokens: &mut Vec<String>| {
        let t = token.trim();
        if !t.is_empty() {
            tokens.push(t.to_string());
        }
        token.clear();
    };

    for ch in inner.chars() {
        if in_string {
            token.push(ch);
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == quote {
                in_string = false;
                flush(&mut token, &mut tokens);
            }
            continue;
        }

        match ch {
            '"' | '\'' => {
                flush(&mut token, &mut tokens);
                in_string = true;
                quote = ch;
                token.push(ch);
            }
            ',' => {
                flush(&mut token, &mut tokens);
            }
            _ => token.push(ch),
        }
    }
    flush(&mut token, &mut tokens);

    for t in tokens {
        if t.starts_with('"') || t.starts_with('\'') {
            let unquoted = t
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
            if kind.is_none() {
                kind = Some(unquoted);
            }
        } else if let Ok(v) = t.parse::<f64>() {
            nums.push(v);
        } else {
            return Err(format!("数値として解釈できません: {t}"));
        }
    }

    let kind = kind.ok_or_else(|| "区間種別 (文字列) が見つかりません".to_string())?;
    Ok((kind, nums))
}

fn split_lua_numbers(s: &str) -> Result<Vec<f64>, String> {
    let mut nums = Vec::new();
    let mut in_string = false;
    let mut quote = '"';
    let mut escape = false;
    let mut token = String::new();

    let flush = |token: &mut String, nums: &mut Vec<f64>| -> Result<(), String> {
        let t = token.trim();
        if t.is_empty() {
            token.clear();
            return Ok(());
        }
        let v: f64 = t
            .parse()
            .map_err(|_| format!("数値として解釈できません: {t}"))?;
        nums.push(v);
        token.clear();
        Ok(())
    };

    for ch in s.chars() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == quote {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' | '\'' => {
                in_string = true;
                quote = ch;
            }
            ',' => flush(&mut token, &mut nums)?,
            _ => token.push(ch),
        }
    }
    flush(&mut token, &mut nums)?;
    Ok(nums)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounce_endpoints() {
        assert!((bounce_out(0.0) - 0.0).abs() < 1e-6);
        assert!((bounce_out(1.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn spring_endpoints() {
        let p = SpringParams::default();
        assert!((eval_spring(0.0, &p) - 0.0).abs() < 1e-6);
        assert!((eval_spring(1.0, &p) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn multi_bezier_endpoints() {
        let anchors = vec![Anchor::new(0.0, 0.0), Anchor::new(1.0, 1.0)];
        assert!((eval_multi_bezier(0.0, &anchors) - 0.0).abs() < 1e-3);
        assert!((eval_multi_bezier(1.0, &anchors) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn composite_default_endpoints() {
        let c = CompositeCurve::default();
        assert!((c.eval(0.0) - 0.0).abs() < 1e-3);
        assert!((c.eval(1.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn composite_multi_segment_mixed_kinds() {
        let mut c = CompositeCurve {
            anchors: vec![Anchor::new(0.0, 0.0), Anchor::new(0.5, 0.5), Anchor::new(1.0, 1.0)],
            segments: vec![
                CompositeSegment {
                    kind: SegmentKind::Linear,
                    ..Default::default()
                },
                CompositeSegment {
                    kind: SegmentKind::Bounce,
                    ..Default::default()
                },
            ],
        };
        // 直線区間の中点は正確に線形補間される。
        assert!((c.eval(0.25) - 0.25).abs() < 1e-6);
        assert!((c.eval(0.0) - 0.0).abs() < 1e-6);
        assert!((c.eval(0.5) - 0.5).abs() < 1e-6);
        assert!((c.eval(1.0) - 1.0).abs() < 1e-3);

        c.insert_anchor_at(0.75, 0.9);
        assert_eq!(c.anchors.len(), 4);
        assert_eq!(c.segments.len(), 3);

        c.remove_anchor(1);
        assert_eq!(c.anchors.len(), 3);
        assert_eq!(c.segments.len(), 2);
    }

    #[test]
    fn composite_segment_index_boundaries() {
        let c = CompositeCurve {
            anchors: vec![Anchor::new(0.0, 0.0), Anchor::new(0.3, 0.3), Anchor::new(1.0, 1.0)],
            segments: vec![CompositeSegment::default(), CompositeSegment::default()],
        };
        assert_eq!(c.segment_index_for(0.0), 0);
        assert_eq!(c.segment_index_for(0.29), 0);
        assert_eq!(c.segment_index_for(0.31), 1);
        assert_eq!(c.segment_index_for(1.0), 1);
    }

    #[test]
    fn multi_lua_roundtrip() {
        let anchors = vec![
            Anchor::new(0.0, 0.0),
            Anchor::new(0.35, 0.8),
            Anchor::new(1.0, 1.0),
        ];
        let lua = generate_lua(
            CurveMode::MultiBezier,
            &BezierParams::default(),
            &SpringParams::default(),
            &anchors,
            &CompositeCurve::default(),
            "",
        );
        let parsed = parse_multi_anchors_from_lua(&lua).expect("parse multi");
        assert_eq!(parsed.len(), 3);
        for (a, b) in parsed.iter().zip(anchors.iter()) {
            assert!((a.t - b.t).abs() < 1e-5);
            assert!((a.y - b.y).abs() < 1e-5);
        }
    }

    #[test]
    fn composite_lua_roundtrip() {
        let composite = CompositeCurve {
            anchors: vec![
                Anchor::new(0.0, 0.0),
                Anchor::new(0.5, 0.5),
                Anchor::new(1.0, 1.0),
            ],
            segments: vec![
                CompositeSegment {
                    kind: SegmentKind::Linear,
                    ..Default::default()
                },
                CompositeSegment {
                    kind: SegmentKind::Spring,
                    zeta: 0.5,
                    omega: 12.0,
                    ..Default::default()
                },
            ],
        };
        let lua = generate_lua(
            CurveMode::Composite,
            &BezierParams::default(),
            &SpringParams::default(),
            &[],
            &composite,
            "",
        );
        let parsed = parse_composite_from_lua(&lua).expect("parse composite");
        assert_eq!(parsed.anchors.len(), composite.anchors.len());
        assert_eq!(parsed.segments.len(), composite.segments.len());
        for (a, b) in parsed.anchors.iter().zip(composite.anchors.iter()) {
            assert!((a.t - b.t).abs() < 1e-5);
            assert!((a.y - b.y).abs() < 1e-5);
        }
        for (a, b) in parsed.segments.iter().zip(composite.segments.iter()) {
            assert_eq!(a.kind, b.kind);
            assert!((a.cx1 - b.cx1).abs() < 1e-5);
            assert!((a.cy1 - b.cy1).abs() < 1e-5);
            assert!((a.cx2 - b.cx2).abs() < 1e-5);
            assert!((a.cy2 - b.cy2).abs() < 1e-5);
            assert!((a.zeta - b.zeta).abs() < 1e-5);
            assert!((a.omega - b.omega).abs() < 1e-5);
        }
    }

    #[test]
    fn parse_invalid_anchors_count() {
        let lua = "local anchors = { { 0, 0 } }";
        assert!(parse_multi_anchors_from_lua(lua).is_err());
    }

    #[test]
    fn bezier_lua_roundtrip() {
        let p = BezierParams {
            y0: 0.0,
            x1: 0.25,
            y1: 0.1,
            x2: 0.25,
            y2: 1.0,
            y3: 1.0,
        };
        let lua = generate_lua(
            CurveMode::Bezier,
            &p,
            &SpringParams::default(),
            &[],
            &CompositeCurve::default(),
            "",
        );
        let parsed = parse_bezier_from_lua(&lua).expect("parse bezier");
        assert!((parsed.x1 - p.x1).abs() < 1e-4);
        assert!((parsed.y1 - p.y1).abs() < 1e-4);
        assert!((parsed.x2 - p.x2).abs() < 1e-4);
        assert!((parsed.y2 - p.y2).abs() < 1e-4);
    }

    #[test]
    fn bezier_same_endpoints_generates_arc_not_constant() {
        let p = BezierParams {
            y0: 0.0,
            x1: 0.25,
            y1: 1.25,
            x2: 0.75,
            y2: 1.25,
            y3: 0.0,
        };
        let lua = generate_bezier_lua(&p);
        assert!(
            !lua.contains("return y0\n") && !lua.trim_end().ends_with("return y0"),
            "y0==y3 must not short-circuit to constant: {lua}"
        );
        assert!(lua.contains("local x1, y1, x2, y2"));
        let mid = eval_bezier(0.5, &p);
        assert!(
            mid > 0.5,
            "arc peak should be well above 0, got {mid}"
        );
        let parsed = parse_bezier_from_lua(&lua).expect("parse absolute bezier");
        assert!((parsed.y0 - p.y0).abs() < 1e-4);
        assert!((parsed.y3 - p.y3).abs() < 1e-4);
        assert!((parsed.y1 - p.y1).abs() < 1e-4);
        assert!((parsed.y2 - p.y2).abs() < 1e-4);
        assert!((eval_bezier(0.5, &parsed) - mid).abs() < 1e-4);
    }

    #[test]
    fn spring_lua_roundtrip() {
        let p = SpringParams {
            zeta: 0.35,
            omega: 14.0,
        };
        let lua = generate_lua(
            CurveMode::Spring,
            &BezierParams::default(),
            &p,
            &[],
            &CompositeCurve::default(),
            "",
        );
        let parsed = parse_spring_from_lua(&lua).expect("parse spring");
        assert!((parsed.zeta - p.zeta).abs() < 1e-4);
        assert!((parsed.omega - p.omega).abs() < 1e-4);
    }

    #[test]
    fn bounce_lua_detect() {
        let lua = generate_lua(
            CurveMode::Bounce,
            &BezierParams::default(),
            &SpringParams::default(),
            &[],
            &CompositeCurve::default(),
            "",
        );
        assert!(parse_bounce_from_lua(&lua).is_ok());
    }

    #[test]
    fn script_preview_detects_bezier_not_composite_inner_bounce() {
        let bezier_lua = "return bezier(t, 0.42, 0.0, 0.58, 1.0)";
        match detect_script_preview(bezier_lua) {
            Some(ScriptPreview::Bezier(p)) => {
                assert!((p.x1 - 0.42).abs() < 1e-6);
            }
            other => panic!("expected bezier, got {other:?}"),
        }

        let composite = CompositeCurve::default();
        let lua = generate_lua(
            CurveMode::Composite,
            &BezierParams::default(),
            &SpringParams::default(),
            &[],
            &composite,
            "",
        );
        match detect_script_preview(&lua) {
            Some(ScriptPreview::Composite(_)) => {}
            other => panic!("expected composite, got {other:?}"),
        }

        assert!(detect_script_preview("return t * t").is_none());
    }

    // --- 反転 ---
    //
    // 期待値は「元のカーブから逆算した鏡像」で書く（au2-script-verification）。
    //   左右反転: flipped(t) == orig(1-t)
    //   上下反転: flipped(t) == (y_start + y_end) - orig(t)
    // 判定は t = 0.00, 0.05, … 1.00 の 21 点すべてで行う。

    /// 判定点。端点も含める。
    fn sample_ts() -> Vec<f64> {
        (0..=20).map(|i| i as f64 * 0.05).collect()
    }

    #[test]
    fn flip_bezier_horizontal_is_time_mirror() {
        let orig = BezierParams { y0: 0.1, x1: 0.2, y1: 0.9, x2: 0.7, y2: 0.3, y3: 1.2 };
        let mut flipped = orig.clone();
        flip_bezier(&mut flipped, FlipAxis::Horizontal);
        for t in sample_ts() {
            let got = eval_bezier(t, &flipped);
            let want = eval_bezier(1.0 - t, &orig);
            assert!((got - want).abs() < 1e-5, "t={t}: {got} != {want}");
        }
    }

    #[test]
    fn flip_bezier_vertical_is_value_mirror() {
        let orig = BezierParams { y0: 0.1, x1: 0.2, y1: 0.9, x2: 0.7, y2: 0.3, y3: 1.2 };
        let sum = orig.y0 + orig.y3;
        let mut flipped = orig.clone();
        flip_bezier(&mut flipped, FlipAxis::Vertical);
        for t in sample_ts() {
            let got = eval_bezier(t, &flipped);
            let want = sum - eval_bezier(t, &orig);
            assert!((got - want).abs() < 1e-5, "t={t}: {got} != {want}");
        }
    }

    #[test]
    fn flip_bezier_twice_is_identity() {
        let orig = BezierParams { y0: -0.2, x1: 0.15, y1: 0.8, x2: 0.9, y2: 0.05, y3: 1.4 };
        for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
            let mut p = orig.clone();
            flip_bezier(&mut p, axis);
            flip_bezier(&mut p, axis);
            assert!((p.y0 - orig.y0).abs() < 1e-12, "{axis:?} y0");
            assert!((p.x1 - orig.x1).abs() < 1e-12, "{axis:?} x1");
            assert!((p.y1 - orig.y1).abs() < 1e-12, "{axis:?} y1");
            assert!((p.x2 - orig.x2).abs() < 1e-12, "{axis:?} x2");
            assert!((p.y2 - orig.y2).abs() < 1e-12, "{axis:?} y2");
            assert!((p.y3 - orig.y3).abs() < 1e-12, "{axis:?} y3");
        }
    }

    /// 左右＋上下（180° 回転）で ease-in が ease-out になる。
    /// CSS の定義値と一致することを確かめる: ease-in(0.42,0,1,1) → ease-out(0,0,0.58,1)。
    #[test]
    fn flip_both_swaps_ease_in_and_out() {
        let mut p = BezierParams { y0: 0.0, x1: 0.42, y1: 0.0, x2: 1.0, y2: 1.0, y3: 1.0 };
        flip_bezier(&mut p, FlipAxis::Horizontal);
        flip_bezier(&mut p, FlipAxis::Vertical);
        assert!((p.y0 - 0.0).abs() < 1e-12);
        assert!((p.x1 - 0.0).abs() < 1e-12);
        assert!((p.y1 - 0.0).abs() < 1e-12);
        assert!((p.x2 - 0.58).abs() < 1e-12);
        assert!((p.y2 - 1.0).abs() < 1e-12);
        assert!((p.y3 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn flip_anchors_matches_mirrors() {
        let orig = vec![
            Anchor::new(0.0, 0.0),
            Anchor::new(0.3, 0.8),
            Anchor::new(0.75, 0.2),
            Anchor::new(1.0, 1.0),
        ];

        let mut h = orig.clone();
        flip_anchors(&mut h, FlipAxis::Horizontal);
        for t in sample_ts() {
            let got = eval_multi_bezier(t, &h);
            let want = eval_multi_bezier(1.0 - t, &orig);
            assert!((got - want).abs() < 1e-5, "H t={t}: {got} != {want}");
        }

        let sum = orig[0].y + orig[orig.len() - 1].y;
        let mut v = orig.clone();
        flip_anchors(&mut v, FlipAxis::Vertical);
        for t in sample_ts() {
            let got = eval_multi_bezier(t, &v);
            let want = sum - eval_multi_bezier(t, &orig);
            assert!((got - want).abs() < 1e-5, "V t={t}: {got} != {want}");
        }
    }

    fn mixed_composite(kind_mid: SegmentKind) -> CompositeCurve {
        CompositeCurve {
            anchors: vec![
                Anchor::new(0.0, 0.0),
                Anchor::new(0.25, 0.6),
                Anchor::new(0.7, 0.35),
                Anchor::new(1.0, 1.0),
            ],
            segments: vec![
                CompositeSegment { kind: SegmentKind::Linear, ..Default::default() },
                CompositeSegment {
                    kind: kind_mid,
                    cx1: 0.1,
                    cy1: 0.9,
                    cx2: 0.8,
                    cy2: 0.2,
                    ..Default::default()
                },
                CompositeSegment { kind: SegmentKind::Bezier, ..Default::default() },
            ],
        }
    }

    #[test]
    fn flip_composite_horizontal_is_time_mirror() {
        // 直線とベジェだけなら厳密に写せる。
        let orig = mixed_composite(SegmentKind::Bezier);
        let mut flipped = orig.clone();
        let unflippable = flip_composite(&mut flipped, FlipAxis::Horizontal);
        assert_eq!(unflippable, 0);
        for t in sample_ts() {
            let got = flipped.eval(t);
            let want = orig.eval(1.0 - t);
            assert!((got - want).abs() < 1e-5, "t={t}: {got} != {want}");
        }
    }

    #[test]
    fn flip_composite_vertical_is_value_mirror_for_all_kinds() {
        // 上下反転は区間の形に触らないので、振動・バウンスを含んでも厳密。
        for kind in SegmentKind::ALL {
            let orig = mixed_composite(kind);
            let sum = orig.anchors[0].y + orig.anchors[orig.anchors.len() - 1].y;
            let mut flipped = orig.clone();
            assert_eq!(flip_composite(&mut flipped, FlipAxis::Vertical), 0);
            for t in sample_ts() {
                let got = flipped.eval(t);
                let want = sum - orig.eval(t);
                assert!((got - want).abs() < 1e-6, "{kind:?} t={t}: {got} != {want}");
            }
        }
    }

    #[test]
    fn flip_composite_horizontal_reports_unflippable_segments() {
        for kind in [SegmentKind::Spring, SegmentKind::Bounce] {
            let mut c = mixed_composite(kind);
            assert_eq!(flip_composite(&mut c, FlipAxis::Horizontal), 1, "{kind:?}");
            // 端点の t は 0/1 のまま、区間数も保たれる。
            assert_eq!(c.segments.len(), 3);
            assert!((c.anchors[0].t - 0.0).abs() < 1e-12);
            assert!((c.anchors[c.anchors.len() - 1].t - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn flip_composite_twice_is_identity() {
        let orig = mixed_composite(SegmentKind::Bezier);
        for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
            let mut c = orig.clone();
            flip_composite(&mut c, axis);
            flip_composite(&mut c, axis);
            for t in sample_ts() {
                let got = c.eval(t);
                let want = orig.eval(t);
                assert!((got - want).abs() < 1e-9, "{axis:?} t={t}: {got} != {want}");
            }
        }
    }

    /// 反転結果が生成 Lua を経由しても同じ形で戻る。
    /// Lua 欄はフォーカスを外すたびに再パースされるので、ここが崩れると
    /// 反転した直後の形が黙って別物になる。
    #[test]
    fn flipped_curves_roundtrip_through_lua() {
        let empty_anchors: Vec<Anchor> = Vec::new();

        // ベジェ: 上下反転で端点が 0/1 から外れ、絶対座標形式の Lua になる。
        let mut p = BezierParams::default();
        flip_bezier(&mut p, FlipAxis::Vertical);
        let lua = generate_lua(
            CurveMode::Bezier,
            &p,
            &SpringParams::default(),
            &empty_anchors,
            &CompositeCurve::default(),
            "",
        );
        let back = parse_bezier_from_lua(&lua).expect("bezier roundtrip");
        assert!((back.y0 - p.y0).abs() < 1e-4, "y0");
        assert!((back.x1 - p.x1).abs() < 1e-4, "x1");
        assert!((back.y1 - p.y1).abs() < 1e-4, "y1");
        assert!((back.x2 - p.x2).abs() < 1e-4, "x2");
        assert!((back.y2 - p.y2).abs() < 1e-4, "y2");
        assert!((back.y3 - p.y3).abs() < 1e-4, "y3");

        // マルチベジェ
        let mut anchors = vec![
            Anchor::new(0.0, 0.0),
            Anchor::new(0.3, 0.8),
            Anchor::new(1.0, 1.0),
        ];
        flip_anchors(&mut anchors, FlipAxis::Horizontal);
        let lua = generate_lua(
            CurveMode::MultiBezier,
            &BezierParams::default(),
            &SpringParams::default(),
            &anchors,
            &CompositeCurve::default(),
            "",
        );
        let back = parse_multi_anchors_from_lua(&lua).expect("multi roundtrip");
        assert_eq!(back.len(), anchors.len());
        for (a, b) in back.iter().zip(anchors.iter()) {
            assert!((a.t - b.t).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4);
        }

        // 標準(複合)
        let mut c = mixed_composite(SegmentKind::Bezier);
        flip_composite(&mut c, FlipAxis::Horizontal);
        let lua = generate_lua(
            CurveMode::Composite,
            &BezierParams::default(),
            &SpringParams::default(),
            &empty_anchors,
            &c,
            "",
        );
        let back = parse_composite_from_lua(&lua).expect("composite roundtrip");
        assert_eq!(back.segments.len(), c.segments.len());
        for t in sample_ts() {
            assert!((back.eval(t) - c.eval(t)).abs() < 1e-4, "t={t}");
        }
    }

    #[test]
    fn mode_supports_flip_excludes_unrepresentable_modes() {
        assert!(mode_supports_flip(CurveMode::Bezier));
        assert!(mode_supports_flip(CurveMode::MultiBezier));
        assert!(mode_supports_flip(CurveMode::Composite));
        assert!(!mode_supports_flip(CurveMode::Spring));
        assert!(!mode_supports_flip(CurveMode::Bounce));
        assert!(!mode_supports_flip(CurveMode::Script));
    }
}
