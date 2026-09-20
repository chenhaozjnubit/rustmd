//! A small but real LaTeX math typesetter.
//!
//! This is not KaTeX: there is no HTML/CSS in a native `egui` app. Instead the
//! input is parsed into a box tree, the box tree is laid out with TeX's
//! horizontal/vertical box model (baselines, math axis, script levels, inter-atom
//! spacing classes) and then painted with an OpenType math font. That gives
//! proper fractions, radicals, big-operator limits, scalable delimiters and
//! aligned scripts — the parts that actually matter visually.
//!
//! Supported: Greek letters, relations and binary operators, `\frac`, `\sqrt`,
//! `\binom`, superscripts/subscripts, primes, `\sum`/`\prod`/`\int` families with
//! limits, `\left...\right` delimiters, accents (`\hat`, `\bar`, `\vec`, …),
//! `\overline`/`\underline`, `\text`/`\mathrm`/`\mathbf`, `\mathbb`-lite, spacing
//! commands, and simple `\begin{matrix}` environments.

use std::sync::Arc;

use egui::{Color32, FontFamily, FontId, Galley, Painter, Pos2, Rect, Stroke, Ui, vec2};

/// Fraction of a font's line box that sits above the baseline. Used for every
/// glyph so that vertical composition stays consistent across sizes.
const ASCENT_RATIO: f32 = 0.775;

const AXIS: f32 = 0.25; // height of the math axis, in em
const RULE_T: f32 = 0.045; // fraction bar thickness, in em

// ===========================================================================
// Laid-out boxes
// ===========================================================================

pub enum Placed {
    Empty,
    Text {
        galley: Arc<Galley>,
        w: f32,
        asc: f32,
        desc: f32,
    },
    Group {
        w: f32,
        asc: f32,
        desc: f32,
        kids: Vec<(f32, f32, Placed)>,
        rects: Vec<(f32, f32, f32, f32)>,
        lines: Vec<(f32, f32, f32, f32, f32)>,
    },
}

impl Placed {
    pub fn w(&self) -> f32 {
        match self {
            Placed::Empty => 0.0,
            Placed::Text { w, .. } | Placed::Group { w, .. } => *w,
        }
    }
    pub fn asc(&self) -> f32 {
        match self {
            Placed::Empty => 0.0,
            Placed::Text { asc, .. } | Placed::Group { asc, .. } => *asc,
        }
    }
    pub fn desc(&self) -> f32 {
        match self {
            Placed::Empty => 0.0,
            Placed::Text { desc, .. } | Placed::Group { desc, .. } => *desc,
        }
    }
    pub fn height(&self) -> f32 {
        self.asc() + self.desc()
    }
}

struct GroupBuilder {
    kids: Vec<(f32, f32, Placed)>,
    rects: Vec<(f32, f32, f32, f32)>,
    lines: Vec<(f32, f32, f32, f32, f32)>,
    w: f32,
    asc: f32,
    desc: f32,
}

impl GroupBuilder {
    fn new() -> Self {
        Self {
            kids: Vec::new(),
            rects: Vec::new(),
            lines: Vec::new(),
            w: 0.0,
            asc: 0.0,
            desc: 0.0,
        }
    }

    /// Add a child whose baseline sits at `dy` relative to this group's baseline.
    fn put(&mut self, dx: f32, dy: f32, child: Placed) {
        let right = dx + child.w();
        if right > self.w {
            self.w = right;
        }
        let above = child.asc() - dy;
        if above > self.asc {
            self.asc = above;
        }
        let below = dy + child.desc();
        if below > self.desc {
            self.desc = below;
        }
        self.kids.push((dx, dy, child));
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.rects.push((x, y, w, h));
        let right = x + w;
        if right > self.w {
            self.w = right;
        }
        if y + h > self.desc {
            self.desc = y + h;
        }
        if -y > self.asc {
            self.asc = -y;
        }
    }

    #[allow(dead_code)]
    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, t: f32) {
        self.lines.push((x1, y1, x2, y2, t));
        for (x, y) in [(x1, y1), (x2, y2)] {
            if x > self.w {
                self.w = x;
            }
            if y + t * 0.5 > self.desc {
                self.desc = y + t * 0.5;
            }
            if -y + t * 0.5 > self.asc {
                self.asc = -y + t * 0.5;
            }
        }
    }

    fn finish(self, w: Option<f32>) -> Placed {
        Placed::Group {
            w: w.unwrap_or(self.w),
            asc: self.asc,
            desc: self.desc,
            kids: self.kids,
            rects: self.rects,
            lines: self.lines,
        }
    }
}

// ===========================================================================
// Atom classes and spacing
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Kind {
    Ord,
    Op,
    Rel,
    Open,
    Close,
    Punct,
    Big,
    Inner,
}

/// TeX's inter-atom spacing table, in em.
fn space_between(a: Kind, b: Kind) -> f32 {
    use Kind::*;
    match (a, b) {
        (Ord, Op) | (Op, Ord) | (Close, Op) | (Op, Open) | (Inner, Op) | (Op, Inner) => 0.1667,
        (Ord, Rel) | (Rel, Ord) | (Close, Rel) | (Rel, Open) | (Inner, Rel) | (Rel, Inner) => 0.2778,
        (Punct, _) => 0.1667,
        (Big, Ord) | (Big, Open) | (Big, Inner) | (Ord, Big) | (Inner, Big) | (Op, Big) => 0.1667,
        _ => 0.0,
    }
}

fn class_of_node(n: &Node) -> Kind {
    match n {
        Node::Atom { class, .. } => *class,
        Node::Seq { class, .. } => *class,
        Node::Big { .. } => Kind::Big,
        _ => Kind::Ord,
    }
}

// ===========================================================================
// AST
// ===========================================================================

#[derive(Debug, Clone)]
enum Node {
    Row(Vec<Node>),
    Atom {
        text: String,
        /// Render with the italic math face.
        italic: bool,
        /// Render with the upright roman text face.
        roman: bool,
        class: Kind,
    },
    Seq {
        text: String,
        class: Kind,
    },
    Frac {
        num: Box<Node>,
        den: Box<Node>,
        bar: bool,
    },
    Sqrt {
        body: Box<Node>,
        index: Option<Box<Node>>,
    },
    Script {
        base: Box<Node>,
        sup: Option<Box<Node>>,
        sub: Option<Box<Node>>,
    },
    Big {
        text: String,
        sup: Option<Box<Node>>,
        sub: Option<Box<Node>>,
    },
    Delim {
        left: String,
        body: Box<Node>,
        right: String,
    },
    Accent {
        text: String,
        base: Box<Node>,
        bar: bool,
    },
    Overline {
        base: Box<Node>,
        under: bool,
    },
    Space(f32),
    /// A grid of cells, used by `matrix` environments.
    Grid(Vec<Vec<Node>>),
}

// ===========================================================================
// Parser
// ===========================================================================

struct P {
    c: Vec<char>,
    i: usize,
}

impl P {
    fn new(s: &str) -> Self {
        Self {
            c: s.chars().collect(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    #[allow(dead_code)]
    fn peek_at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn row(&mut self, stop: &[char]) -> Node {
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            let Some(ch) = self.peek() else { break };
            if stop.contains(&ch) {
                break;
            }
            let before = self.i;
            if let Some(n) = self.scripted() {
                items.push(n);
            }
            if self.i == before {
                self.i += 1; // guarantee progress
            }
        }
        Node::Row(items)
    }

    /// An atom followed by any number of `^` / `_` / `'` markers.
    fn scripted(&mut self) -> Option<Node> {
        let mut base = self.atom()?;
        let mut sup: Option<Box<Node>> = None;
        let mut sub: Option<Box<Node>> = None;
        loop {
            let save = self.i;
            self.skip_ws();
            match self.peek() {
                Some('^') => {
                    self.i += 1;
                    sup = Some(Box::new(self.group_or_atom()));
                }
                Some('_') => {
                    self.i += 1;
                    sub = Some(Box::new(self.group_or_atom()));
                }
                Some('\'') => {
                    self.i += 1;
                    let prime = Node::Atom {
                        text: "\u{2032}".into(),
                        italic: false,
                        roman: true,
                        class: Kind::Ord,
                    };
                    sup = Some(Box::new(match sup.take() {
                        Some(prev) => Node::Row(vec![*prev, prime]),
                        None => prime,
                    }));
                }
                _ => {
                    self.i = save;
                    break;
                }
            }
        }
        if sup.is_some() || sub.is_some() {
            base = Node::Script {
                base: Box::new(base),
                sup,
                sub,
            };
        }
        Some(base)
    }

    fn group_or_atom(&mut self) -> Node {
        self.skip_ws();
        if self.eat('{') {
            let inner = self.row(&['}']);
            self.eat('}');
            inner
        } else if let Some(n) = self.scripted() {
            n
        } else {
            self.skip_ws();
            Node::Row(Vec::new())
        }
    }

    fn atom(&mut self) -> Option<Node> {
        self.skip_ws();
        let ch = self.peek()?;
        match ch {
            '{' => {
                self.i += 1;
                let inner = self.row(&['}']);
                self.eat('}');
                Some(inner)
            }
            '}' | '^' | '_' | '&' => None,
            '\\' => {
                self.i += 1;
                Some(self.command())
            }
            c if c.is_ascii_digit() => {
                let mut s = String::new();
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
                    s.push(self.bump().unwrap());
                }
                Some(Node::Seq {
                    text: s,
                    class: Kind::Ord,
                })
            }
            c if c.is_alphabetic() => {
                self.i += 1;
                Some(Node::Atom {
                    text: c.to_string(),
                    italic: true,
                    roman: false,
                    class: Kind::Ord,
                })
            }
            '(' | '[' => {
                self.i += 1;
                Some(Node::Seq {
                    text: ch.to_string(),
                    class: Kind::Open,
                })
            }
            ')' | ']' => {
                self.i += 1;
                Some(Node::Seq {
                    text: ch.to_string(),
                    class: Kind::Close,
                })
            }
            ',' | ';' => {
                self.i += 1;
                Some(Node::Seq {
                    text: ch.to_string(),
                    class: Kind::Punct,
                })
            }
            '|' => {
                self.i += 1;
                Some(Node::Seq {
                    text: "|".into(),
                    class: Kind::Ord,
                })
            }
            '+' | '-' | '*' | '/' | '=' | '<' | '>' => {
                self.i += 1;
                let (text, class) = match ch {
                    '+' => ("+", Kind::Op),
                    '-' => ("\u{2212}", Kind::Op),
                    '*' => ("\u{22C5}", Kind::Op),
                    '/' => ("/", Kind::Op),
                    '=' => ("=", Kind::Rel),
                    '<' => ("<", Kind::Rel),
                    _ => (">", Kind::Rel),
                };
                Some(Node::Seq {
                    text: text.into(),
                    class,
                })
            }
            '!' | '?' | ':' => {
                self.i += 1;
                Some(Node::Seq {
                    text: ch.to_string(),
                    class: Kind::Ord,
                })
            }
            _ => {
                self.i += 1;
                Some(Node::Seq {
                    text: ch.to_string(),
                    class: Kind::Ord,
                })
            }
        }
    }

    fn braced_text(&mut self) -> String {
        self.skip_ws();
        if self.eat('{') {
            let mut depth = 1;
            let mut s = String::new();
            while let Some(c) = self.bump() {
                match c {
                    '{' => {
                        depth += 1;
                        s.push(c);
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        s.push(c);
                    }
                    _ => s.push(c),
                }
            }
            s
        } else {
            self.bump().map(|c| c.to_string()).unwrap_or_default()
        }
    }

    fn optional_group(&mut self) -> Option<Node> {
        self.skip_ws();
        if self.eat('[') {
            let inner = self.row(&[']']);
            self.eat(']');
            Some(inner)
        } else {
            None
        }
    }

    fn command(&mut self) -> Node {
        // Control word or control symbol.
        let mut name = String::new();
        if matches!(self.peek(), Some(c) if c.is_alphabetic()) {
            while matches!(self.peek(), Some(c) if c.is_alphabetic()) {
                name.push(self.bump().unwrap());
            }
            if self.eat(' ') {
                // a space after a control word is swallowed
            }
        } else if let Some(c) = self.bump() {
            name.push(c);
        }

        match name.as_str() {
            "frac" | "dfrac" | "tfrac" => {
                let a = self.group_or_atom();
                let b = self.group_or_atom();
                Node::Frac {
                    num: Box::new(a),
                    den: Box::new(b),
                    bar: true,
                }
            }
            "binom" | "dbinom" | "tbinom" => {
                let a = self.group_or_atom();
                let b = self.group_or_atom();
                Node::Delim {
                    left: "(".into(),
                    body: Box::new(Node::Frac {
                        num: Box::new(a),
                        den: Box::new(b),
                        bar: false,
                    }),
                    right: ")".into(),
                }
            }
            "sqrt" => {
                let index = self.optional_group();
                let body = self.group_or_atom();
                Node::Sqrt {
                    body: Box::new(body),
                    index: index.map(Box::new),
                }
            }
            "text" | "textrm" | "mathrm" | "operatorname" | "mbox" => {
                let s = self.braced_text();
                Node::Seq {
                    text: s,
                    class: Kind::Ord,
                }
            }
            "mathbf" | "bm" | "boldsymbol" => {
                let s = self.braced_text();
                Node::Atom {
                    text: s,
                    italic: false,
                    roman: true,
                    class: Kind::Ord,
                }
            }
            "mathit" | "textit" => {
                let s = self.braced_text();
                Node::Atom {
                    text: s,
                    italic: true,
                    roman: false,
                    class: Kind::Ord,
                }
            }
            "left" => {
                let l = self.read_delim();
                let body = self.row_until_right();
                Node::Delim {
                    left: l,
                    body: Box::new(body),
                    right: String::new(),
                }
            }
            "right" => {
                let r = self.read_delim();
                Node::Seq {
                    text: r,
                    class: Kind::Close,
                }
            }
            "middle" => {
                let _ = self.read_delim();
                Node::Space(0.0)
            }
            "begin" => {
                let env = self.braced_text();
                self.skip_column_spec(&env);
                let body = self.parse_environment(&env);
                Self::wrap_env(&env, body)
            }
            "end" => {
                let _ = self.braced_text();
                Node::Space(0.0)
            }
            "hat" | "widehat" => self.accent("\u{02C6}"),
            "bar" | "overline" => {
                let base = self.group_or_atom();
                Node::Accent {
                    text: "\u{00AF}".into(),
                    base: Box::new(base),
                    bar: true,
                }
            }
            "underline" => {
                let base = self.group_or_atom();
                Node::Overline {
                    base: Box::new(base),
                    under: true,
                }
            }
            "vec" => self.accent("\u{2192}"),
            "dot" => self.accent("\u{02D9}"),
            "ddot" => self.accent("\u{00A8}"),
            "tilde" | "widetilde" => self.accent("\u{02DC}"),
            "check" => self.accent("\u{02C7}"),
            "breve" => self.accent("\u{02D8}"),
            "acute" => self.accent("\u{00B4}"),
            "grave" => self.accent("\u{0060}"),
            "quad" => Node::Space(1.0),
            "qquad" => Node::Space(2.0),
            "," => Node::Space(0.1667),
            ":" | ";" => Node::Space(0.2778),
            "!" => Node::Space(-0.1667),
            " " => Node::Space(0.25),
            "limits" | "nolimits" | "displaystyle" | "textstyle" | "mathrm2" => Node::Space(0.0),
            _ => {
                if let Some((sym, class)) = symbol(&name) {
                    Node::Seq { text: sym, class }
                } else if let Some((op, _class)) = big_op(&name) {
                    Node::Big {
                        text: op,
                        sup: None,
                        sub: None,
                    }
                } else {
                    // Unknown control sequence: show it upright so the user can
                    // see what went wrong instead of losing the text.
                    Node::Atom {
                        text: name,
                        italic: false,
                        roman: true,
                        class: Kind::Ord,
                    }
                }
            }
        }
    }

    fn read_delim(&mut self) -> String {
        self.skip_ws();
        match self.peek() {
            Some('\\') => {
                self.i += 1;
                let mut n = String::new();
                if matches!(self.peek(), Some(c) if c.is_alphabetic()) {
                    while matches!(self.peek(), Some(c) if c.is_alphabetic()) {
                        n.push(self.bump().unwrap());
                    }
                } else if let Some(c) = self.bump() {
                    n.push(c);
                }
                match n.as_str() {
                    "{" | "lbrace" => "{".into(),
                    "}" | "rbrace" => "}".into(),
                    "langle" => "\u{27E8}".into(),
                    "rangle" => "\u{27E9}".into(),
                    "lvert" | "rvert" | "vert" => "|".into(),
                    "Vert" | "lVert" | "rVert" => "\u{2016}".into(),
                    "lfloor" => "\u{230A}".into(),
                    "rfloor" => "\u{230B}".into(),
                    "lceil" => "\u{2308}".into(),
                    "rceil" => "\u{2309}".into(),
                    "." => "".into(),
                    other => other.to_string(),
                }
            }
            Some('(') => {
                self.i += 1;
                "(".into()
            }
            Some(')') => {
                self.i += 1;
                ")".into()
            }
            Some('[') => {
                self.i += 1;
                "[".into()
            }
            Some(']') => {
                self.i += 1;
                "]".into()
            }
            Some('|') => {
                self.i += 1;
                "|".into()
            }
            Some('.') => {
                self.i += 1;
                "".into()
            }
            Some(c) => {
                self.i += 1;
                c.to_string()
            }
            None => String::new(),
        }
    }

    /// Collect everything up to the matching `\right`, folding any `\middle`
    /// delimiter we encounter into a `Delim` chain.
    fn row_until_right(&mut self) -> Node {
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some('\\') => {
                    // Look ahead for \right / \middle without consuming blindly.
                    let save = self.i;
                    self.i += 1;
                    let mut n = String::new();
                    while matches!(self.peek(), Some(c) if c.is_alphabetic()) {
                        n.push(self.bump().unwrap());
                    }
                    if n == "right" {
                        self.i = save;
                        break;
                    }
                    if n == "middle" {
                        let d = self.read_delim();
                        if !d.is_empty() {
                            items.push(Node::Seq {
                                text: d,
                                class: Kind::Ord,
                            });
                        }
                        continue;
                    }
                    self.i = save;
                    if let Some(node) = self.scripted() {
                        items.push(node);
                    } else {
                        self.i += 1;
                    }
                }
                _ => {
                    let before = self.i;
                    if let Some(node) = self.scripted() {
                        items.push(node);
                    }
                    if self.i == before {
                        self.i += 1;
                    }
                }
            }
        }
        Node::Row(items)
    }

    /// Close the cell under construction and hand it to the current row.
    ///
    /// A cell is one entry in the row vector, because that is the unit
    /// `Node::Grid` aligns: cell `i` of every row is placed at the same x. So
    /// the nodes of a cell have to be wrapped together rather than appended
    /// side by side, otherwise each node becomes its own column.
    fn end_cell(rows: &mut [Vec<Node>], cell: &mut Vec<Node>, force: bool) {
        if cell.is_empty() && !force {
            return;
        }
        let items = std::mem::take(cell);
        let node = match items.len() {
            0 => Node::Space(0.0),
            1 => items.into_iter().next().unwrap(),
            _ => Node::Row(items),
        };
        if let Some(row) = rows.last_mut() {
            row.push(node);
        }
    }

    /// Give a `\begin{env}` body whatever delimiters its name implies.
    ///
    /// `aligned`, `gathered` and bare `matrix` are deliberately absent: they
    /// carry no delimiters of their own.
    fn wrap_env(env: &str, body: Node) -> Node {
        let (left, right) = match env {
            "pmatrix" => ("(", ")"),
            "bmatrix" => ("[", "]"),
            "Bmatrix" => ("{", "}"),
            "vmatrix" => ("|", "|"),
            "Vmatrix" => ("\u{2016}", "\u{2016}"),
            // `cases` is a left brace against an open right side.
            "cases" => ("{", ""),
            _ => return body,
        };
        Node::Delim {
            left: left.into(),
            body: Box::new(body),
            right: right.into(),
        }
    }

    fn parse_environment(&mut self, _env: &str) -> Node {
        let mut rows: Vec<Vec<Node>> = vec![Vec::new()];
        let mut cell: Vec<Node> = Vec::new();

        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some('\\') => {
                    // `\\` is the row separator, and it has to be tested before
                    // the control-word scan below. The second backslash is not
                    // alphabetic, so that scan returns an empty name and the
                    // pair was mistaken for an unknown command — which both
                    // failed to end the row and painted a stray `\`.
                    if self.peek_at(1) == Some('\\') {
                        self.i += 2;
                        Self::end_cell(&mut rows, &mut cell, false);
                        rows.push(Vec::new());
                        continue;
                    }
                    let save = self.i;
                    self.i += 1;
                    let mut n = String::new();
                    while matches!(self.peek(), Some(c) if c.is_alphabetic()) {
                        n.push(self.bump().unwrap());
                    }
                    if n == "end" {
                        self.i = save;
                        break;
                    }
                    if n == "cr" {
                        Self::end_cell(&mut rows, &mut cell, false);
                        rows.push(Vec::new());
                        continue;
                    }
                    self.i = save;
                    if let Some(node) = self.scripted() {
                        cell.push(node);
                    } else {
                        self.i += 1;
                    }
                }
                Some('&') => {
                    self.i += 1;
                    Self::end_cell(&mut rows, &mut cell, true);
                }
                _ => {
                    let before = self.i;
                    if let Some(node) = self.scripted() {
                        cell.push(node);
                    }
                    if self.i == before {
                        self.i += 1;
                    }
                }
            }
        }
        // consume the matching `\end{env}`
        if self.peek() == Some('\\') {
            self.i += 1;
            let mut n = String::new();
            while matches!(self.peek(), Some(c) if c.is_alphabetic()) {
                n.push(self.bump().unwrap());
            }
            let _ = self.braced_text();
        }
        Self::end_cell(&mut rows, &mut cell, false);

        // A trailing `\\` leaves a row with no cells behind.
        rows.retain(|r| !r.is_empty());
        if rows.is_empty() {
            return Node::Space(0.0);
        }
        Node::Grid(rows)
    }

    /// `\begin{array}{cc}` and friends carry a column spec that we do not
    /// honour; swallow it so it cannot surface as literal content.
    fn skip_column_spec(&mut self, env: &str) {
        if !matches!(env, "array" | "tabular" | "tabularx" | "longtable") {
            return;
        }
        let save = self.i;
        self.skip_ws();
        if !self.eat('{') {
            self.i = save;
            return;
        }
        let mut depth = 1;
        while depth > 0 {
            match self.bump() {
                Some('{') => depth += 1,
                Some('}') => depth -= 1,
                Some(_) => {}
                None => break,
            }
        }
    }

    fn accent(&mut self, mark: &str) -> Node {
        let base = self.group_or_atom();
        Node::Accent {
            text: mark.to_string(),
            base: Box::new(base),
            bar: false,
        }
    }
}

fn symbol(name: &str) -> Option<(String, Kind)> {
    let s = |t: &str, k: Kind| Some((t.to_string(), k));
    match name {
        // Greek
        "alpha" => s("\u{03B1}", Kind::Ord),
        "beta" => s("\u{03B2}", Kind::Ord),
        "gamma" => s("\u{03B3}", Kind::Ord),
        "delta" => s("\u{03B4}", Kind::Ord),
        "epsilon" => s("\u{03F5}", Kind::Ord),
        "varepsilon" => s("\u{03B5}", Kind::Ord),
        "zeta" => s("\u{03B6}", Kind::Ord),
        "eta" => s("\u{03B7}", Kind::Ord),
        "theta" => s("\u{03B8}", Kind::Ord),
        "vartheta" => s("\u{03D1}", Kind::Ord),
        "iota" => s("\u{03B9}", Kind::Ord),
        "kappa" => s("\u{03BA}", Kind::Ord),
        "lambda" => s("\u{03BB}", Kind::Ord),
        "mu" => s("\u{03BC}", Kind::Ord),
        "nu" => s("\u{03BD}", Kind::Ord),
        "xi" => s("\u{03BE}", Kind::Ord),
        "pi" => s("\u{03C0}", Kind::Ord),
        "varpi" => s("\u{03D6}", Kind::Ord),
        "rho" => s("\u{03C1}", Kind::Ord),
        "varrho" => s("\u{03F1}", Kind::Ord),
        "sigma" => s("\u{03C3}", Kind::Ord),
        "varsigma" => s("\u{03C2}", Kind::Ord),
        "tau" => s("\u{03C4}", Kind::Ord),
        "upsilon" => s("\u{03C5}", Kind::Ord),
        "phi" => s("\u{03D5}", Kind::Ord),
        "varphi" => s("\u{03C6}", Kind::Ord),
        "chi" => s("\u{03C7}", Kind::Ord),
        "psi" => s("\u{03C8}", Kind::Ord),
        "omega" => s("\u{03C9}", Kind::Ord),
        "Gamma" => s("\u{0393}", Kind::Ord),
        "Delta" => s("\u{0394}", Kind::Ord),
        "Theta" => s("\u{0398}", Kind::Ord),
        "Lambda" => s("\u{039B}", Kind::Ord),
        "Xi" => s("\u{039E}", Kind::Ord),
        "Pi" => s("\u{03A0}", Kind::Ord),
        "Sigma" => s("\u{03A3}", Kind::Ord),
        "Upsilon" => s("\u{03A5}", Kind::Ord),
        "Phi" => s("\u{03A6}", Kind::Ord),
        "Psi" => s("\u{03A8}", Kind::Ord),
        "Omega" => s("\u{03A9}", Kind::Ord),
        // Relations
        "le" | "leq" | "leqslant" => s("\u{2264}", Kind::Rel),
        "ge" | "geq" | "geqslant" => s("\u{2265}", Kind::Rel),
        "ne" | "neq" => s("\u{2260}", Kind::Rel),
        "approx" => s("\u{2248}", Kind::Rel),
        "simeq" => s("\u{2243}", Kind::Rel),
        "cong" => s("\u{2245}", Kind::Rel),
        "equiv" => s("\u{2261}", Kind::Rel),
        "sim" => s("\u{223C}", Kind::Rel),
        "propto" => s("\u{221D}", Kind::Rel),
        "prec" => s("\u{227A}", Kind::Rel),
        "succ" => s("\u{227B}", Kind::Rel),
        "in" => s("\u{2208}", Kind::Rel),
        "notin" => s("\u{2209}", Kind::Rel),
        "ni" => s("\u{220B}", Kind::Rel),
        "subset" => s("\u{2282}", Kind::Rel),
        "supset" => s("\u{2283}", Kind::Rel),
        "subseteq" => s("\u{2286}", Kind::Rel),
        "supseteq" => s("\u{2287}", Kind::Rel),
        "to" | "rightarrow" => s("\u{2192}", Kind::Rel),
        "leftarrow" | "gets" => s("\u{2190}", Kind::Rel),
        "leftrightarrow" => s("\u{2194}", Kind::Rel),
        "Rightarrow" | "implies" => s("\u{21D2}", Kind::Rel),
        "Leftarrow" => s("\u{21D0}", Kind::Rel),
        "Leftrightarrow" | "iff" => s("\u{21D4}", Kind::Rel),
        "mapsto" => s("\u{21A6}", Kind::Rel),
        "uparrow" => s("\u{2191}", Kind::Rel),
        "downarrow" => s("\u{2193}", Kind::Rel),
        "parallel" => s("\u{2225}", Kind::Rel),
        "perp" => s("\u{22A5}", Kind::Rel),
        "mid" => s("\u{2223}", Kind::Rel),
        "models" => s("\u{22A8}", Kind::Rel),
        "asymp" => s("\u{224D}", Kind::Rel),
        // Operators
        "pm" => s("\u{00B1}", Kind::Op),
        "mp" => s("\u{2213}", Kind::Op),
        "times" => s("\u{00D7}", Kind::Op),
        "div" => s("\u{00F7}", Kind::Op),
        "cdot" => s("\u{22C5}", Kind::Op),
        "ast" => s("\u{2217}", Kind::Op),
        "star" => s("\u{22C6}", Kind::Op),
        "circ" => s("\u{2218}", Kind::Op),
        "bullet" => s("\u{2219}", Kind::Op),
        "oplus" => s("\u{2295}", Kind::Op),
        "otimes" => s("\u{2297}", Kind::Op),
        "ominus" => s("\u{2296}", Kind::Op),
        "cup" => s("\u{222A}", Kind::Op),
        "cap" => s("\u{2229}", Kind::Op),
        "setminus" => s("\u{2216}", Kind::Op),
        "wedge" | "land" => s("\u{2227}", Kind::Op),
        "vee" | "lor" => s("\u{2228}", Kind::Op),
        // Ordinary symbols
        "infty" => s("\u{221E}", Kind::Ord),
        "partial" => s("\u{2202}", Kind::Ord),
        "nabla" => s("\u{2207}", Kind::Ord),
        "forall" => s("\u{2200}", Kind::Ord),
        "exists" => s("\u{2203}", Kind::Ord),
        "nexists" => s("\u{2204}", Kind::Ord),
        "emptyset" | "varnothing" => s("\u{2205}", Kind::Ord),
        "angle" => s("\u{2220}", Kind::Ord),
        "triangle" => s("\u{25B3}", Kind::Ord),
        "square" => s("\u{25A1}", Kind::Ord),
        "aleph" => s("\u{2135}", Kind::Ord),
        "hbar" => s("\u{210F}", Kind::Ord),
        "ell" => s("\u{2113}", Kind::Ord),
        "Re" => s("\u{211C}", Kind::Ord),
        "Im" => s("\u{2111}", Kind::Ord),
        "ldots" | "dots" => s("\u{2026}", Kind::Ord),
        "cdots" => s("\u{22EF}", Kind::Ord),
        "vdots" => s("\u{22EE}", Kind::Ord),
        "ddots" => s("\u{22F1}", Kind::Ord),
        "prime" => s("\u{2032}", Kind::Ord),
        "degree" => s("\u{00B0}", Kind::Ord),
        "checkmark" => s("\u{2713}", Kind::Ord),
        "ldots2" => s("\u{2026}", Kind::Ord),
        // Upright function names
        "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "sinh" | "cosh" | "tanh" | "coth"
        | "arcsin" | "arccos" | "arctan" | "log" | "ln" | "lg" | "exp" | "det" | "dim" | "ker"
        | "deg" | "gcd" | "hom" | "arg" | "min" | "max" | "sup" | "inf" | "limsup" | "liminf"
        | "mod" => {
            let t = match name {
                "arcsin" => "\u{2212}",
                _ => "",
            };
            let _ = t;
            Some((name.to_string(), Kind::Ord))
        }
        _ => None,
    }
}

fn big_op(name: &str) -> Option<(String, Kind)> {
    let t = match name {
        "sum" => "\u{2211}",
        "prod" => "\u{220F}",
        "coprod" => "\u{2210}",
        "int" => "\u{222B}",
        "iint" => "\u{222C}",
        "iiint" => "\u{222D}",
        "oint" => "\u{222E}",
        "bigcup" => "\u{22C3}",
        "bigcap" => "\u{22C2}",
        "bigoplus" => "\u{2A01}",
        "bigotimes" => "\u{2A02}",
        "bigvee" => "\u{22C1}",
        "bigwedge" => "\u{22C0}",
        "lim" => "lim",
        "limsup" => "lim sup",
        "liminf" => "lim inf",
        "max" => "max",
        "min" => "min",
        "sup" => "sup",
        "inf" => "inf",
        "det" => "det",
        "gcd" => "gcd",
        _ => return None,
    };
    Some((t.to_string(), Kind::Big))
}

// ===========================================================================
// Layout
// ===========================================================================

struct Ctx<'a> {
    ui: &'a Ui,
    color: Color32,
    display: bool,
}

impl<'a> Ctx<'a> {
    fn font(size: f32, italic: bool) -> FontId {
        let fam = if italic {
            FontFamily::Name("mathit".into())
        } else {
            FontFamily::Name("math".into())
        };
        FontId::new(size, fam)
    }

    fn text(&self, s: &str, size: f32, italic: bool) -> Placed {
        if s.is_empty() {
            return Placed::Empty;
        }
        let galley = self
            .ui
            .fonts(|f| f.layout_no_wrap(s.to_string(), Self::font(size, italic), self.color));
        let h = galley.rect.height();
        let w = galley.rect.width();
        let asc = h * ASCENT_RATIO;
        Placed::Text {
            galley,
            w,
            asc,
            desc: h - asc,
        }
    }

    fn roman_text(&self, s: &str, size: f32) -> Placed {
        if s.is_empty() {
            return Placed::Empty;
        }
        let galley = self.ui.fonts(|f| {
            f.layout_no_wrap(
                s.to_string(),
                FontId::new(size, FontFamily::Name("serif".into())),
                self.color,
            )
        });
        let h = galley.rect.height();
        let w = galley.rect.width();
        let asc = h * ASCENT_RATIO;
        Placed::Text {
            galley,
            w,
            asc,
            desc: h - asc,
        }
    }

    fn layout(&self, node: &Node, size: f32) -> Placed {
        match node {
            Node::Row(items) => {
                let mut gb = GroupBuilder::new();
                let mut x = 0.0f32;
                let mut prev: Option<Kind> = None;
                for it in items {
                    let k = class_of_node(it);
                    if let Some(p) = prev {
                        x += space_between(p, k) * size;
                    }
                    let child = self.layout(it, size);
                    gb.put(x, 0.0, child);
                    x = gb.w;
                    prev = Some(k);
                }
                gb.finish(None)
            }
            Node::Atom {
                text,
                italic,
                roman,
                ..
            } => {
                if *roman && text.chars().count() > 1 {
                    self.roman_text(text, size)
                } else {
                    self.text(text, size, *italic)
                }
            }
            Node::Seq { text, .. } => self.text(text, size, false),
            Node::Space(em) => {
                let w = (em * size).max(0.0);
                let mut gb = GroupBuilder::new();
                gb.w = w;
                gb.finish(Some(w))
            }
            Node::Frac { num, den, bar } => {
                let inner = if self.display { size * 0.96 } else { size * 0.84 };
                let n = self.layout(num, inner);
                let d = self.layout(den, inner);
                let pad_x = size * 0.14;
                let pad_y = size * 0.16;
                let rule = if *bar { (size * RULE_T).max(0.55) } else { 0.0 };
                let w = n.w().max(d.w()) + pad_x * 2.0;
                let mut gb = GroupBuilder::new();
                gb.put(
                    (w - n.w()) * 0.5,
                    -(pad_y + rule * 0.5 + n.desc()),
                    n,
                );
                gb.put(
                    (w - d.w()) * 0.5,
                    pad_y + rule * 0.5 + d.asc(),
                    d,
                );
                if *bar {
                    gb.rect(0.0, -rule * 0.5, w, rule);
                }
                let out = gb.finish(Some(w));
                // The fraction baseline is its bar; shift down onto the math axis
                // when it participates in a larger expression.
                let mut outer = GroupBuilder::new();
                outer.put(0.0, size * AXIS, out);
                outer.finish(None)
            }
            Node::Sqrt { body, index } => {
                let b = self.layout(body, size);
                let body_h = b.height().max(size);
                let scale = (body_h / (size * ASCENT_RATIO)).max(1.0);
                let rad = self.text("\u{221A}", size * scale, false);
                let rad_w = rad.w();
                let thick = (size * 0.05).max(0.6);
                let bar_y = -(b.asc() + size * 0.14);
                let mut gb = GroupBuilder::new();
                gb.put(0.0, 0.0, rad);
                gb.put(rad_w, 0.0, b);
                // the vinculum, overlapping the radical's own top-right serif
                gb.rect(rad_w * 0.72, bar_y, rad_w * 0.28 + gb.kids[1].2.w(), thick);
                if let Some(ix) = index {
                    let s = self.layout(ix, size * 0.62);
                    let sw = s.w();
                    gb.put(-sw * 0.4, bar_y - s.desc() - size * 0.04, s);
                }
                gb.finish(None)
            }
            Node::Script { base, sup, sub } => {
                let b = self.layout(base, size);
                let bw = b.w();
                let ssize = size * 0.7;
                let sup_p = sup.as_ref().map(|s| self.layout(s, ssize));
                let sub_p = sub.as_ref().map(|s| self.layout(s, ssize));
                let sup_dy = -(b.asc() * 0.62).max(size * 0.42);
                let sub_dy = (b.desc() * 0.30).max(size * 0.14) + size * 0.06;
                let mut gb = GroupBuilder::new();
                gb.put(0.0, 0.0, b);
                if let Some(s) = sup_p {
                    gb.put(bw + size * 0.02, sup_dy, s);
                }
                if let Some(s) = sub_p {
                    gb.put(bw + size * 0.02, sub_dy, s);
                }
                gb.finish(None)
            }
            Node::Big { text, sup, sub } => {
                let gsize = size * 1.45;
                let mut b = self.text(text, gsize, false);
                if let Placed::Text { asc, .. } = &mut b {
                    *asc = (gsize * ASCENT_RATIO).min(*asc);
                }
                let ssize = size * 0.68;
                let sup_p = sup.as_ref().map(|s| self.layout(s, ssize));
                let sub_p = sub.as_ref().map(|s| self.layout(s, ssize));
                let limits = self.display && (sup_p.is_some() || sub_p.is_some());
                let mut gb = GroupBuilder::new();
                gb.put(0.0, 0.0, b);
                let bw = gb.kids[0].2.w();
                if limits {
                    if let Some(s) = &sup_p {
                        let s = Self::clone_placed(s);
                        let sw = s.w();
                        gb.put((bw - sw) * 0.5, -(gsize * 0.62 + s.desc()), s);
                    }
                    if let Some(s) = &sub_p {
                        let s = Self::clone_placed(s);
                        let sw = s.w();
                        gb.put((bw - sw) * 0.5, gsize * 0.20 + s.asc(), s);
                    }
                } else {
                    let mut x = bw + size * 0.02;
                    if let Some(s) = &sup_p {
                        gb.put(x, -(gsize * 0.42), Self::clone_placed(s));
                        x += sup_p.as_ref().unwrap().w();
                    }
                    if let Some(s) = &sub_p {
                        gb.put(x, gsize * 0.10, Self::clone_placed(s));
                    }
                }
                gb.finish(None)
            }
            Node::Delim { left, body, right } => {
                let b = self.layout(body, size);
                let want = (b.height() / ASCENT_RATIO).max(size);
                let mk = |s: &str| -> Placed {
                    if s.is_empty() {
                        Placed::Empty
                    } else {
                        self.text(s, want, false)
                    }
                };
                let mut gb = GroupBuilder::new();
                let l = mk(left);
                gb.put(0.0, 0.0, l);
                let lw = gb.w;
                gb.put(lw + size * 0.05, 0.0, b);
                let x = gb.w;
                if !right.is_empty() {
                    gb.put(x + size * 0.05, 0.0, mk(right));
                }
                gb.finish(None)
            }
            Node::Accent { text, base, bar } => {
                let b = self.layout(base, size);
                let bw = b.w();
                let b_asc = b.asc();
                let mut gb = GroupBuilder::new();
                gb.put(0.0, 0.0, b);
                if *bar {
                    let thick = (size * 0.05).max(0.6);
                    gb.rect(0.0, -(b_asc + size * 0.16), bw, thick);
                } else {
                    let a = self.text(text, size * 0.95, false);
                    let aw = a.w();
                    gb.put((bw - aw) * 0.5, -(b_asc + size * 0.16), a);
                }
                gb.finish(None)
            }
            Node::Overline { base, under } => {
                let b = self.layout(base, size);
                let bw = b.w();
                let b_asc = b.asc();
                let b_desc = b.desc();
                let thick = (size * 0.05).max(0.6);
                let mut gb = GroupBuilder::new();
                let dy = if *under { size * 0.16 } else { 0.0 };
                let y = if *under {
                    b_desc + size * 0.16
                } else {
                    -(b_asc + size * 0.16)
                };
                gb.put(0.0, dy, b);
                gb.rect(0.0, y, bw, thick);
                gb.finish(None)
            }
            Node::Grid(rows) => {
                let ssize = size * 0.94;
                let laid: Vec<Vec<Placed>> = rows
                    .iter()
                    .map(|r| r.iter().map(|n| self.layout(n, ssize)).collect())
                    .collect();
                let ncol = laid.iter().map(|r| r.len()).max().unwrap_or(0);
                let mut col_w = vec![0.0f32; ncol];
                for r in &laid {
                    for (i, c) in r.iter().enumerate() {
                        col_w[i] = col_w[i].max(c.w());
                    }
                }
                let gap = size * 0.8;
                let mut gb = GroupBuilder::new();
                let mut y = 0.0f32;
                for r in &laid {
                    let asc = r.iter().map(|c| c.asc()).fold(0.0f32, f32::max);
                    let desc = r.iter().map(|c| c.desc()).fold(0.0f32, f32::max);
                    let mut x = 0.0f32;
                    for (i, c) in r.iter().enumerate() {
                        gb.put(x, y, Self::clone_placed(c));
                        x += col_w[i] + gap;
                    }
                    y += asc + desc + size * 0.25;
                }
                gb.finish(None)
            }
        }
    }

    /// `Placed` owns an `Arc<Galley>`; cloning is cheap and lets us build the
    /// same subtree in more than one place (used for script placement).
    fn clone_placed(p: &Placed) -> Placed {
        match p {
            Placed::Empty => Placed::Empty,
            Placed::Text {
                galley,
                w,
                asc,
                desc,
            } => Placed::Text {
                galley: galley.clone(),
                w: *w,
                asc: *asc,
                desc: *desc,
            },
            Placed::Group {
                w,
                asc,
                desc,
                kids,
                rects,
                lines,
            } => Placed::Group {
                w: *w,
                asc: *asc,
                desc: *desc,
                kids: kids
                    .iter()
                    .map(|(dx, dy, k)| (*dx, *dy, Self::clone_placed(k)))
                    .collect(),
                rects: rects.clone(),
                lines: lines.clone(),
            },
        }
    }
}

// ===========================================================================
// Public entry points
// ===========================================================================

pub struct MathBox {
    pub placed: Placed,
    /// Width of the whole formula.
    pub width: f32,
    /// Height above the baseline.
    pub ascent: f32,
    /// Height below the baseline.
    pub descent: f32,
}

impl MathBox {
    pub fn height(&self) -> f32 {
        self.ascent + self.descent
    }
}

/// Lay out a formula so the caller can measure it before painting.
pub fn layout(ui: &Ui, tex: &str, size: f32, color: Color32, display: bool) -> MathBox {
    let ctx = Ctx {
        ui,
        color,
        display,
    };
    let mut p = P::new(tex);
    let node = p.row(&[]);
    let node = match &node {
        Node::Row(items) if items.len() == 1 => items[0].clone(),
        other => other.clone(),
    };
    let placed = ctx.layout(&node, size);
    MathBox {
        width: placed.w(),
        ascent: placed.asc(),
        descent: placed.desc(),
        placed,
    }
}

/// Paint a formula with its baseline at `origin`.
pub fn paint(painter: &Painter, origin: Pos2, mb: &MathBox, color: Color32) {
    paint_placed(painter, origin, &mb.placed, color);
}

fn paint_placed(painter: &Painter, at: Pos2, p: &Placed, color: Color32) {
    match p {
        Placed::Empty => {}
        Placed::Text { galley, asc, .. } => {
            painter.galley(Pos2::new(at.x, at.y - asc), galley.clone(), color);
        }
        Placed::Group {
            kids,
            rects,
            lines,
            ..
        } => {
            for (dx, dy, k) in kids {
                paint_placed(painter, Pos2::new(at.x + dx, at.y + dy), k, color);
            }
            for (x, y, w, h) in rects {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(at.x + x, at.y + y), vec2(*w, *h)),
                    0.0,
                    color,
                );
            }
            for (x1, y1, x2, y2, t) in lines {
                painter.line_segment(
                    [
                        Pos2::new(at.x + x1, at.y + y1),
                        Pos2::new(at.x + x2, at.y + y2),
                    ],
                    Stroke::new(*t, color),
                );
            }
        }
    }
}

/// Convenience: measure and paint centred inside `rect`, returning the used size.
#[allow(dead_code)]
pub fn draw_centered(
    ui: &Ui,
    rect: Rect,
    tex: &str,
    size: f32,
    color: Color32,
    display: bool,
) -> egui::Vec2 {
    let mb = layout(ui, tex, size, color, display);
    let start = Pos2::new(
        rect.left() + ((rect.width() - mb.width) * 0.5).max(0.0),
        rect.top() + ((rect.height() - mb.height()) * 0.5).max(0.0) + mb.ascent,
    );
    paint(ui.painter(), start, &mb, color);
    vec2(mb.width, mb.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_commands() {
        let mut p = P::new("\\frac{1}{2} + \\sqrt{x^2 + y^2}");
        let n = p.row(&[]);
        assert!(format!("{n:?}").contains("Frac"));
        assert!(format!("{n:?}").contains("Sqrt"));
    }

    #[test]
    fn superscripts_and_subscripts() {
        let mut p = P::new("x_i^2");
        let n = p.row(&[]);
        let dbg = format!("{n:?}");
        assert!(dbg.contains("sup: Some"), "{dbg}");
        assert!(dbg.contains("sub: Some"), "{dbg}");
    }

    #[test]
    fn greek_and_big_ops() {
        let mut p = P::new("\\sum_{i=1}^{n} \\alpha_i");
        let n = p.row(&[]);
        let dbg = format!("{n:?}");
        assert!(dbg.contains('\u{2211}'), "{dbg}");
        assert!(dbg.contains('\u{03B1}'), "{dbg}");
    }

    #[test]
    fn left_right_delimiters() {
        let mut p = P::new("\\left( \\frac{a}{b} \\right)");
        let n = p.row(&[]);
        assert!(format!("{n:?}").contains("Delim"));
    }

    /// `\\` must end a row and `&` must end a cell. Getting this wrong used to
    /// collapse every matrix, `cases` and `aligned` onto one line, with a
    /// literal `\` painted where the row break should have been.
    #[test]
    fn environment_row_and_cell_breaks() {
        let mut p = P::new("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}");
        let n = p.row(&[]);
        let dbg = format!("{n:?}");
        let grid = match &n {
            Node::Row(items) => items.iter().find_map(|i| match i {
                Node::Delim { body, .. } => Some((**body).clone()),
                _ => None,
            }),
            _ => None,
        }
        .unwrap_or_else(|| panic!("pmatrix should be wrapped in a delimiter: {dbg}"));

        let rows = match &grid {
            Node::Grid(rows) => rows,
            other => panic!("expected a Grid, got {other:#?}"),
        };
        assert_eq!(rows.len(), 2, "expected two rows: {dbg}");
        assert_eq!(rows[0].len(), 2, "expected two cells per row: {dbg}");
        assert_eq!(rows[1].len(), 2, "expected two cells per row: {dbg}");
        // No row break should leak through as a literal backslash.
        assert!(!dbg.contains("text: \"\\\\\""), "stray backslash: {dbg}");
    }

    /// The delimiters a `\begin{…}` name implies must actually be drawn.
    #[test]
    fn environment_delimiters() {
        let cases = [
            ("pmatrix", "(", ")"),
            ("bmatrix", "[", "]"),
            ("vmatrix", "|", "|"),
        ];
        for (env, left, right) in cases {
            let src = format!("\\begin{{{env}}} a & b \\\\ c & d \\end{{{env}}}");
            let mut p = P::new(&src);
            let n = p.row(&[]);
            let found = match &n {
                Node::Row(items) => items.iter().find_map(|i| match i {
                    Node::Delim {
                        left: l,
                        right: r,
                        ..
                    } => Some((l.clone(), r.clone())),
                    _ => None,
                }),
                _ => None,
            };
            let (l, r) = found.unwrap_or_else(|| panic!("{env}: no Delim, got {n:#?}"));
            assert_eq!((l.as_str(), r.as_str()), (left, right), "{env}");
        }

        // `cases` is a left brace with an open right side.
        let mut p = P::new("\\begin{cases} x^2, & x \\ge 0 \\\\ -x, & x < 0 \\end{cases}");
        let n = p.row(&[]);
        let found = match &n {
            Node::Row(items) => items.iter().find_map(|i| match i {
                Node::Delim { left, right, .. } => Some((left.clone(), right.clone())),
                _ => None,
            }),
            _ => None,
        };
        assert_eq!(found, Some(("{".to_string(), String::new())), "{n:#?}");
    }

    /// `aligned` carries no delimiters, and the `{cc}` of `array` must not
    /// surface as literal content.
    #[test]
    fn array_column_spec_is_swallowed() {
        let mut p = P::new("\\begin{array}{cc} a & b \\\\ c & d \\end{array}");
        let n = p.row(&[]);
        let dbg = format!("{n:?}");
        assert!(!dbg.contains("Delim"), "array must not gain delimiters: {dbg}");
        let grid = match &n {
            Node::Row(items) => items.iter().find_map(|i| match i {
                Node::Grid(rows) => Some(rows),
                _ => None,
            }),
            _ => None,
        }
        .unwrap_or_else(|| panic!("expected a Grid, got {n:#?}"));
        let rows = grid;
        assert_eq!(rows.len(), 2, "{dbg}");
        // If `{cc}` had leaked it would sit in front of `a` in the first cell.
        match &rows[0][0] {
            Node::Atom { text, .. } => assert_eq!(text, "a", "column spec leaked: {dbg}"),
            other => panic!("unexpected first cell {other:#?} in {dbg}"),
        }

        let mut p = P::new("\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}");
        let n = p.row(&[]);
        let dbg = format!("{n:?}");
        assert!(!dbg.contains("Delim"), "aligned must not gain delimiters: {dbg}");
        let rows = match &n {
            Node::Row(items) => items.iter().find_map(|i| match i {
                Node::Grid(rows) => Some(rows),
                _ => None,
            }),
            _ => None,
        }
        .unwrap_or_else(|| panic!("expected a Grid, got {n:#?}"));
        assert_eq!(rows.len(), 2, "{dbg}");
    }

    #[test]
    fn unterminated_input_does_not_hang() {
        for s in ["{", "\\frac{1}", "\\left(", "$", "\\sqrt[", "x^"] {
            let mut p = P::new(s);
            let _ = p.row(&[]);
        }
    }
}
