use std::collections::HashMap;
use std::env;
use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_4};
use std::fs;
use std::hash::{BuildHasherDefault, Hasher};
use std::time::{Duration, Instant};

use crate::diag::Span;
use crate::ir::*;
use crate::simulator::matrix::C64;
use crate::simulator::state::Rng;
use crate::transpile::GateSet;

const DIM: usize = 8;
const CELLS: usize = DIM * DIM;
const LETTERS: usize = 33;
const OCCUPIED: u64 = 1 << 63;
const SPELLING: u64 = (1 << 48) - 1;
const FRAME: u32 = 56;
const PERMS: [[u8; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];
const SCALE: f64 = 16384.0;
const TOLERANCE: f64 = 1e-9;
const REACH: usize = 96;
const MAX_BARRIER: usize = 24;
const MIN_GATES: usize = 8;
const SIDEWAYS: f64 = 0.5;
const SKIP: f64 = 0.1;
const WINDOW: usize = 9;
const EXTRA: usize = 2;
const DEPTH: usize = 6;
const ROUNDS: u32 = 8;
const SEED: u64 = 1;

type Mat = [C64; CELLS];

#[derive(Default, Clone, Copy)]
struct Plain(u64);

impl Hasher for Plain {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

type Keyed<V> = HashMap<u64, V, BuildHasherDefault<Plain>>;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Letter {
    H(u8),
    X(u8),
    Rz(u8, u8),
    Cx(u8, u8),
}

impl Letter {
    fn inverse(self) -> Letter {
        match self {
            Letter::Rz(q, k) => Letter::Rz(q, 8 - k),
            other => other,
        }
    }

    fn entangles(self) -> bool {
        matches!(self, Letter::Cx(..))
    }

    fn odd(self) -> bool {
        matches!(self, Letter::Rz(_, k) if k % 2 == 1)
    }
}

fn alphabet() -> Vec<Letter> {
    let mut all = Vec::with_capacity(LETTERS);
    for q in 0..3 {
        all.push(Letter::H(q));
    }
    for q in 0..3 {
        all.push(Letter::X(q));
    }
    for q in 0..3 {
        for k in 1..8 {
            all.push(Letter::Rz(q, k));
        }
    }
    for c in 0..3 {
        for t in 0..3 {
            if c != t {
                all.push(Letter::Cx(c, t));
            }
        }
    }
    all
}

fn turn(k: u8) -> C64 {
    let r = FRAC_1_SQRT_2;
    match k % 8 {
        0 => C64::new(1.0, 0.0),
        1 => C64::new(r, r),
        2 => C64::new(0.0, 1.0),
        3 => C64::new(-r, r),
        4 => C64::new(-1.0, 0.0),
        5 => C64::new(-r, -r),
        6 => C64::new(0.0, -1.0),
        _ => C64::new(r, -r),
    }
}

fn identity() -> Mat {
    let mut m = [C64::new(0.0, 0.0); CELLS];
    for i in 0..DIM {
        m[i * DIM + i] = C64::new(1.0, 0.0);
    }
    m
}

fn apply(m: &mut Mat, letter: Letter) {
    match letter {
        Letter::H(q) => {
            let bit = 1usize << q;
            for row in 0..DIM {
                if row & bit == 0 {
                    let other = row | bit;
                    for column in 0..DIM {
                        let (x, y) = (m[row * DIM + column], m[other * DIM + column]);
                        m[row * DIM + column] = (x + y) * FRAC_1_SQRT_2;
                        m[other * DIM + column] = (x - y) * FRAC_1_SQRT_2;
                    }
                }
            }
        }
        Letter::X(q) => {
            let bit = 1usize << q;
            for row in 0..DIM {
                if row & bit == 0 {
                    let other = row | bit;
                    for column in 0..DIM {
                        m.swap(row * DIM + column, other * DIM + column);
                    }
                }
            }
        }
        Letter::Rz(q, k) => {
            let bit = 1usize << q;
            let phase = turn(k);
            for row in 0..DIM {
                if row & bit != 0 {
                    for column in 0..DIM {
                        m[row * DIM + column] *= phase;
                    }
                }
            }
        }
        Letter::Cx(c, t) => {
            let (control, target) = (1usize << c, 1usize << t);
            for row in 0..DIM {
                if row & control != 0 && row & target == 0 {
                    let other = row | target;
                    for column in 0..DIM {
                        m.swap(row * DIM + column, other * DIM + column);
                    }
                }
            }
        }
    }
}

fn adjoint(m: &Mat) -> Mat {
    let mut out = [C64::new(0.0, 0.0); CELLS];
    for row in 0..DIM {
        for column in 0..DIM {
            out[column * DIM + row] = m[row * DIM + column].conj();
        }
    }
    out
}

fn origins() -> [[usize; DIM]; 6] {
    let mut tables = [[0usize; DIM]; 6];
    for (table, perm) in tables.iter_mut().zip(PERMS) {
        for index in 0..DIM {
            let moved = (0..3).fold(0, |acc, q| acc | ((index >> q) & 1) << perm[q]);
            table[moved] = index;
        }
    }
    tables
}

fn key(m: &Mat, origin: &[usize; DIM]) -> u64 {
    let mut pivot = C64::new(1.0, 0.0);
    'find: for row in origin {
        for column in origin {
            let c = m[row * DIM + column];
            if c.norm_sqr() > 1e-12 {
                pivot = c;
                break 'find;
            }
        }
    }
    let phase = pivot.conj() / pivot.norm();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for row in origin {
        for column in origin {
            let v = m[row * DIM + column] * phase;
            for part in [v.re, v.im] {
                let x = (part * SCALE).round() as i64 as u64;
                h = (h.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95);
            }
        }
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

fn spell(word: &[Letter]) -> Mat {
    let mut m = identity();
    for &letter in word {
        apply(&mut m, letter);
    }
    m
}

fn same(a: &Mat, b: &Mat) -> bool {
    let Some(pivot) = (0..CELLS).max_by(|&x, &y| a[x].norm_sqr().total_cmp(&a[y].norm_sqr()))
    else {
        return false;
    };
    if a[pivot].norm() < 1e-6 || b[pivot].norm() < 1e-6 {
        return false;
    }
    let phase = b[pivot] / a[pivot];
    (phase.norm() - 1.0).abs() < TOLERANCE
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| (x * phase - y).norm() < TOLERANCE)
}

fn canonical(m: &Mat, origins: &[[usize; DIM]; 6]) -> (u64, usize) {
    let mut best = (key(m, &origins[0]), 0);
    for (index, origin) in origins.iter().enumerate().skip(1) {
        let k = key(m, origin);
        if k < best.0 {
            best = (k, index);
        }
    }
    best
}

fn unpack(word: u64, letters: &[Letter]) -> Vec<Letter> {
    let mut out = Vec::new();
    let mut rest = word & SPELLING;
    while rest & 63 != 0 {
        out.push(letters[(rest & 63) as usize - 1]);
        rest >>= 6;
    }
    out
}

fn redundant(last: Option<Letter>, next: Letter) -> bool {
    match (last, next) {
        (Some(Letter::H(a)), Letter::H(b)) | (Some(Letter::X(a)), Letter::X(b)) => a == b,
        (Some(Letter::Rz(a, _)), Letter::Rz(b, _)) => a == b,
        (Some(Letter::Cx(a, b)), Letter::Cx(c, d)) => a == c && b == d,
        _ => false,
    }
}

struct Oracle {
    letters: Vec<Letter>,
    origins: [[usize; DIM]; 6],
    slots: Vec<u64>,
    mask: usize,
    depth: usize,
    absent: Keyed<u8>,
}

impl Oracle {
    fn new(depth: usize, disk: bool) -> Oracle {
        let path = disk.then(|| env::temp_dir().join(format!("qirc_anneal_v3_d{depth}.bin")));
        let slots = path
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .filter(|bytes| {
                bytes.len() >= 32 && bytes.len() % 16 == 0 && (bytes.len() / 16).is_power_of_two()
            })
            .map(|bytes| {
                bytes
                    .chunks_exact(8)
                    .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8])))
                    .collect::<Vec<u64>>()
            })
            .unwrap_or_else(|| {
                let slots = Oracle::build(depth);
                if let Some(path) = &path {
                    let bytes: Vec<u8> = slots.iter().flat_map(|v| v.to_le_bytes()).collect();
                    let staging = path.with_extension(format!("{}.tmp", std::process::id()));
                    if fs::write(&staging, bytes).is_ok() && fs::rename(&staging, path).is_err() {
                        let _ = fs::remove_file(&staging);
                    }
                }
                slots
            });
        Oracle {
            letters: alphabet(),
            origins: origins(),
            mask: slots.len() / 2 - 1,
            slots,
            depth,
            absent: Keyed::default(),
        }
    }

    fn build(depth: usize) -> Vec<u64> {
        let letters = alphabet();
        let origins = origins();
        let mut seen: Keyed<u64> = Keyed::default();
        seen.insert(canonical(&identity(), &origins).0, 0);
        let mut frontier = vec![0u64];
        for level in 0..depth {
            let mut next: Keyed<(u64, u32)> = Keyed::default();
            for &word in &frontier {
                let spelled = unpack(word, &letters);
                let mut base = identity();
                for &letter in &spelled {
                    apply(&mut base, letter);
                }
                let entangled = spelled.iter().filter(|l| l.entangles()).count() as u32;
                let last = spelled.last().copied();
                for (index, &letter) in letters.iter().enumerate() {
                    if redundant(last, letter) {
                        continue;
                    }
                    let mut m = base;
                    apply(&mut m, letter);
                    let (k, frame) = canonical(&m, &origins);
                    if seen.contains_key(&k) {
                        continue;
                    }
                    let longer = (word & SPELLING)
                        | ((index as u64 + 1) << (6 * level))
                        | ((frame as u64) << FRAME);
                    let cx = entangled + u32::from(letter.entangles());
                    next.entry(k)
                        .and_modify(|held| {
                            if cx < held.1 {
                                *held = (longer, cx);
                            }
                        })
                        .or_insert((longer, cx));
                }
            }
            frontier = next.values().map(|&(word, _)| word).collect();
            seen.extend(next.into_iter().map(|(k, (word, _))| (k, word)));
        }

        let size = (seen.len() * 2).next_power_of_two().max(2);
        let mut slots = vec![0u64; size * 2];
        for (k, word) in seen {
            let mut at = k as usize & (size - 1);
            while slots[2 * at + 1] & OCCUPIED != 0 {
                at = (at + 1) & (size - 1);
            }
            slots[2 * at] = k;
            slots[2 * at + 1] = word | OCCUPIED;
        }
        slots
    }

    fn lookup(&self, m: &Mat) -> Option<Vec<Letter>> {
        let (k, frame) = canonical(m, &self.origins);
        let mut at = k as usize & self.mask;
        let word = loop {
            let word = self.slots[2 * at + 1];
            if word & OCCUPIED == 0 {
                return None;
            }
            if self.slots[2 * at] == k {
                break word;
            }
            at = (at + 1) & self.mask;
        };
        let stored = PERMS[((word >> FRAME) & 7) as usize % 6];
        let wanted = PERMS[frame];
        let place = |q: u8| {
            let through = stored[q as usize];
            wanted.iter().position(|&w| w == through).unwrap_or(0) as u8
        };
        Some(
            unpack(word, &self.letters)
                .into_iter()
                .map(|letter| match letter {
                    Letter::H(q) => Letter::H(place(q)),
                    Letter::X(q) => Letter::X(place(q)),
                    Letter::Rz(q, k) => Letter::Rz(place(q), k),
                    Letter::Cx(c, t) => Letter::Cx(place(c), place(t)),
                })
                .collect(),
        )
    }

    fn best(&mut self, u: &Mat, budget: usize, extra: usize, rng: &mut Rng) -> Option<Vec<Letter>> {
        if let Some(word) = self.lookup(u) {
            return (word.len() <= budget).then_some(word);
        }
        let k = key(u, &self.origins[0]);
        let levels = extra.min(budget.saturating_sub(self.depth));
        let known = self.absent.get(&k).copied().unwrap_or(0) as usize;
        if levels <= known {
            return None;
        }
        let found = self.suffixed(u, known, levels, rng);
        if found.is_none() {
            self.absent.insert(k, levels as u8);
        }
        found
    }

    fn suffixed(&self, u: &Mat, known: usize, levels: usize, rng: &mut Rng) -> Option<Vec<Letter>> {
        let offset = rng.next_u64() as usize % LETTERS;
        for level in known + 1..=levels {
            let mut tail = Vec::with_capacity(level);
            let mut best: Option<(u32, Vec<Letter>)> = None;
            self.descend(u, level, offset, &mut tail, &mut best);
            if let Some((_, word)) = best {
                return Some(word);
            }
        }
        None
    }

    fn descend(
        &self,
        m: &Mat,
        left: usize,
        offset: usize,
        tail: &mut Vec<Letter>,
        best: &mut Option<(u32, Vec<Letter>)>,
    ) {
        if left == 0 {
            if let Some(mut spelled) = self.lookup(m) {
                spelled.extend(tail.iter().rev().copied());
                let cx = spelled.iter().filter(|l| l.entangles()).count() as u32;
                if best.as_ref().is_none_or(|(held, _)| cx < *held) {
                    *best = Some((cx, spelled));
                }
            }
            return;
        }
        for step in 0..LETTERS {
            let letter = self.letters[(step + offset) % LETTERS];
            if redundant(tail.last().copied(), letter) {
                continue;
            }
            let mut peeled = *m;
            apply(&mut peeled, letter.inverse());
            tail.push(letter);
            self.descend(&peeled, left - 1, offset, tail, best);
            tail.pop();
        }
    }
}

#[derive(Clone, Debug)]
enum Node {
    H(u32),
    X(u32),
    Rz(u32, u8),
    Phase(u32, f64),
    Cx(u32, u32),
    Opaque(Box<Op>),
}

impl Node {
    fn of(op: Op) -> Node {
        let simple = |gate: &Gate| {
            let target = gate.targets.first()?.0;
            match (gate.kind, gate.controls.as_slice(), gate.targets.len()) {
                (GateKind::H, [], 1) => Some(Node::H(target)),
                (GateKind::X, [], 1) => Some(Node::X(target)),
                (GateKind::X, [control], 1) if control.0 != target => {
                    Some(Node::Cx(control.0, target))
                }
                (GateKind::Rz, [], 1) => {
                    let angle = gate.constant_angle()?;
                    if !angle.is_finite() {
                        return None;
                    }
                    let steps = (angle / FRAC_PI_4).round();
                    if (angle - steps * FRAC_PI_4).abs() < 1e-10 {
                        match steps.rem_euclid(8.0) as u8 {
                            0 => Some(Node::Phase(target, 0.0)),
                            k => Some(Node::Rz(target, k)),
                        }
                    } else {
                        Some(Node::Phase(target, angle))
                    }
                }
                _ => None,
            }
        };
        match &op {
            Op::Gate(gate) if gate.has_valid_shape() => {
                simple(gate).unwrap_or_else(|| Node::Opaque(Box::new(op)))
            }
            _ => Node::Opaque(Box::new(op)),
        }
    }

    fn op(self) -> Op {
        let gate = |kind, controls: Vec<u32>, target: u32, params| {
            Op::Gate(Gate {
                kind,
                controls: controls.into_iter().map(QubitId).collect(),
                targets: vec![QubitId(target)],
                params,
                span: Span::DUMMY,
            })
        };
        let angle = |value: f64| vec![Operand::Const(Const::Float(value))];
        match self {
            Node::H(q) => gate(GateKind::H, vec![], q, vec![]),
            Node::X(q) => gate(GateKind::X, vec![], q, vec![]),
            Node::Cx(c, t) => gate(GateKind::X, vec![c], t, vec![]),
            Node::Rz(q, k) => {
                let steps = if k > 4 {
                    f64::from(k) - 8.0
                } else {
                    f64::from(k)
                };
                gate(GateKind::Rz, vec![], q, angle(steps * FRAC_PI_4))
            }
            Node::Phase(q, value) => gate(GateKind::Rz, vec![], q, angle(value)),
            Node::Opaque(op) => *op,
        }
    }

    fn wires(&self) -> (u32, Option<u32>) {
        match self {
            Node::H(q) | Node::X(q) | Node::Rz(q, _) | Node::Phase(q, _) => (*q, None),
            Node::Cx(c, t) => (*c, Some(*t)),
            Node::Opaque(_) => (u32::MAX, None),
        }
    }

    fn touches(&self, wires: &[u32]) -> bool {
        let (a, b) = self.wires();
        wires.contains(&a) || b.is_some_and(|b| wires.contains(&b))
    }

    fn inside(&self, wires: &[u32]) -> bool {
        let (a, b) = self.wires();
        wires.contains(&a) && b.is_none_or(|b| wires.contains(&b))
    }

    fn letter(&self, wires: &[u32]) -> Option<Letter> {
        let local = |q: &u32| wires.iter().position(|w| w == q).map(|p| p as u8);
        Some(match self {
            Node::H(q) => Letter::H(local(q)?),
            Node::X(q) => Letter::X(local(q)?),
            Node::Rz(q, k) => Letter::Rz(local(q)?, *k),
            Node::Cx(c, t) => Letter::Cx(local(c)?, local(t)?),
            Node::Phase(..) | Node::Opaque(_) => return None,
        })
    }

    fn placed(letter: Letter, wires: &[u32]) -> Node {
        let at = |q: u8| wires[q as usize];
        match letter {
            Letter::H(q) => Node::H(at(q)),
            Letter::X(q) => Node::X(at(q)),
            Letter::Rz(q, k) => Node::Rz(at(q), k),
            Letter::Cx(c, t) => Node::Cx(at(c), at(t)),
        }
    }

    fn commutes(&self, other: &Node) -> bool {
        use Node::*;
        match (self, other) {
            (Opaque(_), _) | (_, Opaque(_)) => false,
            (Cx(c, t), Cx(d, u)) => c != u && t != d,
            (Cx(_, t), Rz(q, _) | Phase(q, _)) | (Rz(q, _) | Phase(q, _), Cx(_, t)) => q != t,
            (Cx(c, _), X(q)) | (X(q), Cx(c, _)) => q != c,
            (Cx(c, t), H(q)) | (H(q), Cx(c, t)) => q != c && q != t,
            (H(_), H(_)) | (X(_), X(_)) => true,
            (Rz(..) | Phase(..), Rz(..) | Phase(..)) => true,
            (a, b) => a.wires().0 != b.wires().0,
        }
    }
}

struct Window {
    anchor: usize,
    wires: Vec<u32>,
    picked: Vec<usize>,
    old: Vec<Letter>,
    u: Mat,
}

struct Walk<'a> {
    nodes: Vec<Node>,
    qubits: u32,
    oracle: &'a mut Oracle,
    rng: Rng,
}

impl Walk<'_> {
    fn pick_wires(&mut self, anchor: usize) -> Option<Vec<u32>> {
        let (a, b) = self.nodes[anchor].wires();
        let mut wires = vec![a];
        wires.extend(b);
        let low = anchor.saturating_sub(REACH);
        let high = (anchor + REACH + 1).min(self.nodes.len());
        while wires.len() < 3 {
            let mut pool: Vec<u32> = Vec::new();
            for node in &self.nodes[low..high] {
                if matches!(node, Node::Opaque(_)) || !node.touches(&wires) {
                    continue;
                }
                let (x, y) = node.wires();
                for q in [Some(x), y].into_iter().flatten() {
                    if !wires.contains(&q) && !pool.contains(&q) {
                        pool.push(q);
                    }
                }
            }
            if pool.is_empty() {
                if self.qubits < 3 {
                    return None;
                }
                let q = (self.rng.next_u64() % u64::from(self.qubits)) as u32;
                if !wires.contains(&q) {
                    wires.push(q);
                }
            } else {
                wires.push(pool[self.rng.next_u64() as usize % pool.len()]);
            }
        }
        for i in (1..wires.len()).rev() {
            wires.swap(i, self.rng.next_u64() as usize % (i + 1));
        }
        Some(wires)
    }

    fn sweep(
        &mut self,
        anchor: usize,
        wires: &[u32],
        forward: bool,
        room: usize,
        picked: &mut Vec<usize>,
    ) {
        let mut barrier: Vec<usize> = Vec::new();
        let mut taken = 0;
        for offset in 1..=REACH {
            if taken == room || barrier.len() == MAX_BARRIER {
                break;
            }
            let index = if forward {
                anchor + offset
            } else if let Some(index) = anchor.checked_sub(offset) {
                index
            } else {
                break;
            };
            let Some(node) = self.nodes.get(index) else {
                break;
            };
            if matches!(node, Node::Opaque(_)) {
                break;
            }
            if !node.touches(wires) {
                continue;
            }
            let fits = node.inside(wires)
                && !matches!(node, Node::Phase(..))
                && barrier.iter().all(|&b| self.nodes[b].commutes(node));
            if fits && self.rng.next_unit() >= SKIP {
                picked.push(index);
                taken += 1;
            } else {
                barrier.push(index);
            }
        }
    }

    fn window(&mut self) -> Option<Window> {
        let anchor = self.rng.next_u64() as usize % self.nodes.len();
        if !matches!(
            self.nodes[anchor],
            Node::H(_) | Node::X(_) | Node::Rz(..) | Node::Cx(..)
        ) {
            return None;
        }
        let wires = self.pick_wires(anchor)?;
        let cap = 2 + self.rng.next_u64() as usize % (WINDOW - 1);
        let ahead = self.rng.next_u64() as usize % cap;
        let mut picked = vec![anchor];
        if self.rng.next_u64() & 1 == 0 {
            self.sweep(anchor, &wires, true, ahead, &mut picked);
            let room = cap - picked.len();
            self.sweep(anchor, &wires, false, room, &mut picked);
        } else {
            self.sweep(anchor, &wires, false, ahead, &mut picked);
            let room = cap - picked.len();
            self.sweep(anchor, &wires, true, room, &mut picked);
        }
        if picked.len() < 2 {
            return None;
        }
        picked.sort_unstable();
        let old: Vec<Letter> = picked
            .iter()
            .map(|&i| self.nodes[i].letter(&wires))
            .collect::<Option<Vec<_>>>()?;
        let u = spell(&old);
        Some(Window {
            anchor,
            wires,
            picked,
            old,
            u,
        })
    }

    fn step(&mut self) -> bool {
        let Some(w) = self.window() else {
            return false;
        };
        let Window {
            anchor,
            wires,
            picked,
            old,
            u,
        } = w;

        let flipped = self.rng.next_u64() & 1 == 0;
        let query = if flipped { adjoint(&u) } else { u };
        let Some(mut found) = self.oracle.best(&query, old.len(), EXTRA, &mut self.rng) else {
            return false;
        };
        if flipped {
            found.reverse();
            for letter in &mut found {
                *letter = letter.inverse();
            }
        }

        let weigh = |word: &[Letter]| {
            (
                word.len(),
                word.iter().filter(|l| l.entangles()).count(),
                word.iter().filter(|l| l.odd()).count(),
            )
        };
        let (before, after) = (weigh(&old), weigh(&found));
        if after.2 > before.2 || (after.0, after.1) > (before.0, before.1) {
            return false;
        }
        let improved = (after.0, after.1) < (before.0, before.1);
        if !improved && (found == old || self.rng.next_unit() >= SIDEWAYS) {
            return false;
        }
        let mut check = identity();
        for &letter in &found {
            apply(&mut check, letter);
        }
        if !same(&u, &check) {
            return false;
        }

        let position = picked.iter().filter(|&&i| i < anchor).count();
        let slot = anchor - position;
        let mut index = 0;
        let mut cursor = 0;
        self.nodes.retain(|_| {
            let drop = cursor < picked.len() && picked[cursor] == index;
            if drop {
                cursor += 1;
            }
            index += 1;
            !drop
        });
        self.nodes.splice(
            slot..slot,
            found.iter().map(|&letter| Node::placed(letter, &wires)),
        );
        improved
    }
}

type Sum = (i32, Vec<(u8, f64)>);

struct Rule {
    left: Vec<Node>,
    right: Vec<Node>,
    before: Vec<u32>,
    vars: Vec<Option<u8>>,
    sums: Vec<Option<Sum>>,
}

struct Macro {
    stages: Vec<Rule>,
}

const UNBOUND: u32 = u32::MAX;
const BINDINGS: usize = 8;
const MATCH_WORK: u32 = 4000;
const ANGLES: usize = 32;

struct Found {
    picked: Vec<usize>,
    bound: [u32; BINDINGS],
    angles: [f64; ANGLES],
    slot: usize,
}

fn read_macros(text: &str) -> Vec<Macro> {
    type Atom = (Node, Option<u8>, Option<Sum>);
    let word = |text: &str| -> Option<Vec<Atom>> {
        text.split(',')
            .map(str::trim)
            .filter(|gate| !gate.is_empty())
            .map(|gate| {
                let mut parts = gate.split_whitespace();
                let name = parts.next()?;
                let a: u32 = parts.next()?.parse().ok()?;
                Some(match name {
                    "h" => (Node::H(a), None, None),
                    "x" => (Node::X(a), None, None),
                    "cx" => (Node::Cx(a, parts.next()?.parse().ok()?), None, None),
                    "rv" => {
                        let index: u8 = parts.next()?.parse().ok()?;
                        if usize::from(index) >= ANGLES {
                            return None;
                        }
                        (Node::Rz(a, 0), Some(index), None)
                    }
                    "ra" => {
                        let fixed: i32 = parts.next()?.parse().ok()?;
                        let terms = parts
                            .map(|term| {
                                let index: u8 = term.get(1..)?.parse().ok()?;
                                let sign = if term.starts_with('-') { -1.0 } else { 1.0 };
                                (usize::from(index) < ANGLES).then_some((index, sign))
                            })
                            .collect::<Option<Vec<_>>>()?;
                        (Node::Rz(a, 0), None, Some((fixed, terms)))
                    }
                    _ => (
                        Node::Rz(a, name.strip_prefix('r')?.parse().ok()?),
                        None,
                        None,
                    ),
                })
            })
            .collect()
    };
    let span = |nodes: &[Node]| {
        nodes
            .iter()
            .map(|node| {
                let (a, b) = node.wires();
                a.max(b.unwrap_or(0)) as usize + 1
            })
            .max()
            .unwrap_or(0)
    };
    let cx = |nodes: &[Node]| nodes.iter().filter(|n| matches!(n, Node::Cx(..))).count() as i64;
    let mut macros = Vec::new();
    'lines: for line in text.lines() {
        let mut stages = Vec::new();
        let mut known = [false; BINDINGS];
        let (mut gates, mut wide) = (0i64, 0i64);
        for stage in line.split(';') {
            let Some((left, right)) = stage.split_once('|') else {
                continue 'lines;
            };
            let (Some(left), Some(right)) = (word(left), word(right)) else {
                continue 'lines;
            };
            let vars: Vec<Option<u8>> = left.iter().map(|atom| atom.1).collect();
            let sums: Vec<Option<Sum>> = right.iter().map(|atom| atom.2.clone()).collect();
            let left: Vec<Node> = left.into_iter().map(|atom| atom.0).collect();
            let right: Vec<Node> = right.into_iter().map(|atom| atom.0).collect();
            if left.is_empty() || left.len() > 32 || span(&left) > BINDINGS {
                continue 'lines;
            }
            for node in &left {
                let (a, b) = node.wires();
                known[a as usize] = true;
                if let Some(b) = b {
                    known[b as usize] = true;
                }
            }
            let covered = right.iter().all(|node| {
                let (a, b) = node.wires();
                [Some(a), b]
                    .into_iter()
                    .flatten()
                    .all(|q| known.get(q as usize).copied().unwrap_or(false))
            });
            if !covered {
                continue 'lines;
            }
            gates += right.len() as i64 - left.len() as i64;
            wide += cx(&right) - cx(&left);
            let before = (0..left.len())
                .map(|i| {
                    (0..i)
                        .filter(|&j| !left[j].commutes(&left[i]))
                        .fold(0u32, |mask, j| mask | 1 << j)
                })
                .collect();
            stages.push(Rule {
                left,
                right,
                before,
                vars,
                sums,
            });
        }
        if !stages.is_empty() && (gates, wide) < (0, 0) {
            macros.push(Macro { stages });
        }
    }
    macros
}

struct Matcher<'a> {
    nodes: &'a [Node],
    rule: &'a Rule,
    bound: [u32; BINDINGS],
    angles: [f64; ANGLES],
    picked: Vec<usize>,
    barrier: Vec<usize>,
    done: u32,
    work: u32,
}

impl Matcher<'_> {
    fn unify(&mut self, index: usize, node: &Node) -> Option<[(u32, u32); 2]> {
        let pattern = &self.rule.left[index];
        if let (Some(var), Node::Rz(p, _)) = (self.rule.vars[index], pattern) {
            let (q, angle) = match node {
                Node::Rz(q, k) => (*q, f64::from(*k) * FRAC_PI_4),
                Node::Phase(q, angle) => (*q, *angle),
                _ => return None,
            };
            let now = self.bound[*p as usize];
            let free = now == UNBOUND && !self.bound.contains(&q);
            if now != q && !free {
                return None;
            }
            self.angles[usize::from(var)] = angle;
            return Some([(*p, q), (*p, q)]);
        }
        let pair = |p: u32, q: u32| -> Option<(u32, u32)> {
            let now = self.bound[p as usize];
            let free = now == UNBOUND && !self.bound.contains(&q);
            (now == q || free).then_some((p, q))
        };
        match (pattern, node) {
            (Node::H(p), Node::H(q)) | (Node::X(p), Node::X(q)) => {
                let one = pair(*p, *q)?;
                Some([one, one])
            }
            (Node::Rz(p, k), Node::Rz(q, l)) if k == l => {
                let one = pair(*p, *q)?;
                Some([one, one])
            }
            (Node::Cx(p, r), Node::Cx(q, s)) => Some([pair(*p, *q)?, pair(*r, *s)?]),
            _ => None,
        }
    }

    fn extend(&mut self, at: usize) -> bool {
        if self.done.count_ones() as usize == self.rule.left.len() {
            return true;
        }
        if at >= self.nodes.len() || at > self.picked[0] + REACH || self.work == 0 {
            return false;
        }
        self.work -= 1;
        let node = &self.nodes[at];
        if matches!(node, Node::Opaque(_)) {
            return false;
        }
        if self.barrier.iter().all(|&b| self.nodes[b].commutes(node)) {
            for i in 0..self.rule.left.len() {
                if self.done & 1 << i != 0 || self.rule.before[i] & !self.done != 0 {
                    continue;
                }
                let Some(pairs) = self.unify(i, node) else {
                    continue;
                };
                let saved = self.bound;
                for (p, q) in pairs {
                    self.bound[p as usize] = q;
                }
                self.done |= 1 << i;
                self.picked.push(at);
                if self.extend(at + 1) {
                    return true;
                }
                self.picked.pop();
                self.done &= !(1 << i);
                self.bound = saved;
            }
        }
        if self.barrier.is_empty() && self.picked.iter().all(|&p| self.nodes[p].commutes(node)) {
            return self.extend(at + 1);
        }
        self.barrier.push(at);
        if self.extend(at + 1) {
            return true;
        }
        self.barrier.pop();
        false
    }
}

fn find(nodes: &[Node], rule: &Rule, bound: [u32; BINDINGS], anchor: usize) -> Option<Found> {
    for first in 0..rule.left.len() {
        if rule.before[first] != 0 {
            continue;
        }
        let mut matcher = Matcher {
            nodes,
            rule,
            bound,
            angles: [0.0; ANGLES],
            picked: Vec::new(),
            barrier: Vec::new(),
            done: 0,
            work: MATCH_WORK,
        };
        let Some(pairs) = matcher.unify(first, &nodes[anchor]) else {
            continue;
        };
        for (p, q) in pairs {
            matcher.bound[p as usize] = q;
        }
        matcher.done = 1 << first;
        matcher.picked.push(anchor);
        if matcher.extend(anchor + 1) {
            let last = matcher.picked[matcher.picked.len() - 1] + 1;
            let wall = matcher.barrier.first().copied().unwrap_or(last);
            let slot = wall - matcher.picked.iter().filter(|&&p| p < wall).count();
            return Some(Found {
                picked: matcher.picked,
                bound: matcher.bound,
                angles: matcher.angles,
                slot,
            });
        }
    }
    None
}

fn replace(nodes: &mut Vec<Node>, rule: &Rule, found: &Found) {
    let at = |q: u32| found.bound[q as usize];
    let fresh: Vec<Node> = rule
        .right
        .iter()
        .zip(&rule.sums)
        .filter_map(|(node, sum)| {
            Some(match (node, sum) {
                (Node::Rz(q, _), Some((fixed, terms))) => {
                    let start = f64::from(*fixed) * FRAC_PI_4;
                    let angle = terms.iter().fold(start, |sum, (var, sign)| {
                        sum + sign * found.angles[usize::from(*var)]
                    });
                    let steps = (angle / FRAC_PI_4).round();
                    if (angle - steps * FRAC_PI_4).abs() < 1e-10 {
                        match steps.rem_euclid(8.0) as u8 {
                            0 => return None,
                            k => Node::Rz(at(*q), k),
                        }
                    } else {
                        Node::Phase(at(*q), angle)
                    }
                }
                (Node::H(q), _) => Node::H(at(*q)),
                (Node::X(q), _) => Node::X(at(*q)),
                (Node::Rz(q, k), _) => Node::Rz(at(*q), *k),
                (Node::Cx(c, t), _) => Node::Cx(at(*c), at(*t)),
                (other, _) => other.clone(),
            })
        })
        .collect();
    let (mut index, mut cursor) = (0, 0);
    nodes.retain(|_| {
        let drop = cursor < found.picked.len() && found.picked[cursor] == index;
        if drop {
            cursor += 1;
        }
        index += 1;
        !drop
    });
    nodes.splice(found.slot..found.slot, fresh);
}

fn attempt(nodes: &[Node], stages: &[Rule], anchor: usize) -> Option<Vec<Node>> {
    let mut found = find(nodes, &stages[0], [UNBOUND; BINDINGS], anchor)?;
    let low = anchor.saturating_sub(2 * REACH);
    let high = (anchor + 3 * REACH).min(nodes.len());
    let shift = |found: &mut Found| {
        found.slot -= low;
        for index in &mut found.picked {
            *index -= low;
        }
    };
    let mut work = nodes[low..high].to_vec();
    shift(&mut found);
    replace(&mut work, &stages[0], &found);
    let mut middle = found.slot;
    let mut bound = found.bound;
    for stage in &stages[1..] {
        let from = middle.saturating_sub(REACH / 2);
        let upto = (middle + REACH / 2).min(work.len());
        let next = (from..upto).find_map(|start| find(&work, stage, bound, start))?;
        replace(&mut work, stage, &next);
        middle = next.slot;
        bound = next.bound;
    }
    let mut whole = Vec::with_capacity(nodes.len());
    whole.extend_from_slice(&nodes[..low]);
    whole.append(&mut work);
    whole.extend_from_slice(&nodes[high..]);
    Some(whole)
}

fn rewrite(nodes: &mut Vec<Node>, macros: &[Macro]) -> usize {
    let mut applied = 0;
    let mut anchor = 0;
    while anchor < nodes.len() {
        let hit = macros
            .iter()
            .find_map(|rule| attempt(nodes, &rule.stages, anchor));
        let Some(fresh) = hit else {
            anchor += 1;
            continue;
        };
        *nodes = fresh;
        applied += 1;
        anchor = anchor.saturating_sub(2 * REACH);
    }
    applied
}

pub fn distill(program: &mut Program, mut polish: impl FnMut(&mut Program)) -> usize {
    let macros = read_macros(include_str!("distill_rules.txt"));
    if macros.is_empty() {
        return 0;
    }
    let before = weight(program).0;
    for _ in 0..64 {
        let mut applied = 0;
        for block in &mut program.blocks {
            let mut nodes: Vec<Node> = block.ops.drain(..).map(Node::of).collect();
            applied += rewrite(&mut nodes, &macros);
            block.ops = nodes
                .into_iter()
                .filter(|node| !matches!(node, Node::Phase(_, angle) if *angle == 0.0))
                .map(Node::op)
                .collect();
        }
        if applied == 0 {
            break;
        }
        let walked = weight(program);
        let held = program.clone();
        polish(program);
        if weight(program) > walked || foreign(program) > foreign(&held) {
            *program = held;
        }
    }
    before.saturating_sub(weight(program).0)
}

pub fn usable(set: &GateSet) -> bool {
    set.has(GateKind::H, 0)
        && set.has(GateKind::X, 0)
        && set.has(GateKind::Rz, 0)
        && set.has(GateKind::X, 1)
}

fn simple(program: &Program) -> Vec<usize> {
    program
        .blocks
        .iter()
        .map(|block| {
            block
                .ops
                .iter()
                .filter(|op| !matches!(Node::of((*op).clone()), Node::Opaque(_)))
                .count()
        })
        .collect()
}

fn weight(program: &Program) -> (usize, usize) {
    let gates = program
        .blocks
        .iter()
        .flat_map(|block| &block.ops)
        .filter_map(Op::as_gate);
    gates.fold((0, 0), |(all, wide), gate| {
        (all + 1, wide + usize::from(gate.wires().count() > 1))
    })
}

fn foreign(program: &Program) -> usize {
    program.op_count() - simple(program).iter().sum::<usize>()
}

pub fn search(
    program: &mut Program,
    time: Duration,
    mut polish: impl FnMut(&mut Program),
) -> usize {
    let total: usize = simple(program).iter().sum();
    if time.is_zero() || program.num_qubits < 3 || total < MIN_GATES {
        return 0;
    }

    let mut oracle = Oracle::new(DEPTH, true);
    let deadline = Instant::now() + time;
    let before = weight(program).0;

    for round in 0..u64::MAX {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let slice = (now + time / ROUNDS).min(deadline);
        let stalled = walk(program, &mut oracle, slice, SEED.wrapping_add(round));
        let walked = weight(program);
        let held = program.clone();
        polish(program);
        if weight(program) > walked || foreign(program) > foreign(&held) {
            *program = held;
        }
        if stalled && weight(program) == walked {
            break;
        }
    }
    before.saturating_sub(weight(program).0)
}

fn walk(program: &mut Program, oracle: &mut Oracle, deadline: Instant, seed: u64) -> bool {
    let sizes = simple(program);
    let total: usize = sizes.iter().sum();
    let began = Instant::now();
    let mut stalled = true;
    let mut used = Duration::ZERO;

    for (block, size) in program.blocks.iter_mut().zip(sizes) {
        if size < MIN_GATES {
            continue;
        }
        used += deadline
            .saturating_duration_since(began)
            .mul_f64(size as f64 / total as f64);
        let until = began + used;
        let nodes: Vec<Node> = block.ops.drain(..).map(Node::of).collect();
        let mut walk = Walk {
            nodes,
            qubits: program.num_qubits,
            oracle: &mut *oracle,
            rng: Rng::new(seed),
        };
        let mut steps = 0u64;
        let mut idle = 0u64;
        let mut quiet = false;
        while !walk.nodes.is_empty() {
            if steps.is_multiple_of(64) && Instant::now() >= until {
                break;
            }
            steps += 1;
            if walk.step() {
                idle = 0;
            } else {
                idle += 1;
                if idle >= 20_000 + 2_000 * walk.nodes.len() as u64 {
                    quiet = true;
                    break;
                }
            }
        }
        stalled &= quiet;
        block.ops = walk
            .nodes
            .into_iter()
            .filter(|node| !matches!(node, Node::Phase(_, angle) if *angle == 0.0))
            .map(Node::op)
            .collect();
    }
    stalled
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: usize = 5;

    fn random_node(rng: &mut Rng) -> Node {
        let q = (rng.next_u64() % WIDTH as u64) as u32;
        let other = (q + 1 + (rng.next_u64() % (WIDTH as u64 - 1)) as u32) % WIDTH as u32;
        match rng.next_u64() % 10 {
            0 | 1 => Node::H(q),
            2 => Node::X(q),
            3..=5 => Node::Rz(q, 1 + (rng.next_u64() % 7) as u8),
            6 => Node::Phase(q, 0.3),
            _ => Node::Cx(q, other),
        }
    }

    fn evolve(nodes: &[Node]) -> Vec<Vec<C64>> {
        let dim = 1usize << WIDTH;
        (0..dim)
            .map(|basis| {
                let mut v = vec![C64::new(0.0, 0.0); dim];
                v[basis] = C64::new(1.0, 0.0);
                for node in nodes {
                    match *node {
                        Node::H(q) => {
                            for i in 0..dim {
                                if i >> q & 1 == 0 {
                                    let (x, y) = (v[i], v[i | 1 << q]);
                                    v[i] = (x + y) * FRAC_1_SQRT_2;
                                    v[i | 1 << q] = (x - y) * FRAC_1_SQRT_2;
                                }
                            }
                        }
                        Node::X(q) => {
                            for i in 0..dim {
                                if i >> q & 1 == 0 {
                                    v.swap(i, i | 1 << q);
                                }
                            }
                        }
                        Node::Rz(q, k) => {
                            for (i, cell) in v.iter_mut().enumerate() {
                                if i >> q & 1 == 1 {
                                    *cell *= turn(k);
                                }
                            }
                        }
                        Node::Phase(q, angle) => {
                            for (i, cell) in v.iter_mut().enumerate() {
                                if i >> q & 1 == 1 {
                                    *cell *= C64::from_polar(1.0, angle);
                                }
                            }
                        }
                        Node::Cx(c, t) => {
                            for i in 0..dim {
                                if i >> c & 1 == 1 && i >> t & 1 == 0 {
                                    v.swap(i, i | 1 << t);
                                }
                            }
                        }
                        Node::Opaque(_) => unreachable!(),
                    }
                }
                v
            })
            .collect()
    }

    fn agree(a: &[Vec<C64>], b: &[Vec<C64>]) -> bool {
        let pivot = (0..a[0].len())
            .max_by(|&x, &y| a[0][x].norm().total_cmp(&a[0][y].norm()))
            .unwrap();
        let phase = b[0][pivot] / a[0][pivot];
        a.iter()
            .zip(b)
            .all(|(x, y)| x.iter().zip(y).all(|(p, q)| (p * phase - q).norm() < 1e-7))
    }

    #[test]
    fn commutation_rules_match_matrices() {
        let letters = alphabet();
        let wires = [0u32, 1, 2];
        for &a in &letters {
            for &b in &letters {
                let (x, y) = (Node::placed(a, &wires), Node::placed(b, &wires));
                if x.commutes(&y) {
                    let (mut ab, mut ba) = (identity(), identity());
                    apply(&mut ab, a);
                    apply(&mut ab, b);
                    apply(&mut ba, b);
                    apply(&mut ba, a);
                    assert!(same(&ab, &ba), "{a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn oracle_words_are_shortest_and_exact() {
        let mut oracle = Oracle::new(3, false);
        let mut rng = Rng::new(7);
        let letters = alphabet();
        for _ in 0..3000 {
            let size = 1 + rng.next_u64() as usize % 5;
            let word: Vec<Letter> = (0..size)
                .map(|_| letters[rng.next_u64() as usize % LETTERS])
                .collect();
            let mut u = identity();
            for &letter in &word {
                apply(&mut u, letter);
            }
            let found = oracle.best(&u, size, 2, &mut rng).expect("within reach");
            assert!(found.len() <= size);
            let mut check = identity();
            for &letter in &found {
                apply(&mut check, letter);
            }
            assert!(same(&u, &check));
        }
    }

    #[test]
    fn random_walk_preserves_the_unitary() {
        let mut oracle = Oracle::new(3, false);
        let mut rng = Rng::new(11);
        let mut shrunk = 0;
        for round in 0..12 {
            let nodes: Vec<Node> = (0..60).map(|_| random_node(&mut rng)).collect();
            let before = evolve(&nodes);
            let size = nodes.len();
            let mut walk = Walk {
                nodes,
                qubits: WIDTH as u32,
                oracle: &mut oracle,
                rng: Rng::new(round),
            };
            for _ in 0..1500 {
                walk.step();
            }
            assert!(walk.nodes.len() <= size);
            shrunk += size - walk.nodes.len();
            assert!(agree(&before, &evolve(&walk.nodes)), "round {round}");
        }
        assert!(shrunk > 0);
    }
}
