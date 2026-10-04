//! A compiled filter: a small boolean tree over one domain's leaves (DESIGN.md 5.7).
//!
//! `Expr<L> = Const | Leaf | Not | All | Any`. A filter's terms become leaves (or constants, when
//! a term's answer is known at load), and [`Expr::fold`] then simplifies the tree: constants
//! propagate, nested `All`s and `Any`s flatten, and leaves that can be merged are, so that
//! `{Military} {Land}` over base units is one set test.

use super::super::table::CondDeps;

/// A leaf of a filter tree: one test a domain knows how to answer.
///
/// The defaults merge nothing; a leaf type overrides what is exact for it. `and` and `or` must
/// return a leaf equal to the conjunction or disjunction of the two, for every world.
pub trait Leaf: Clone + PartialEq {
    /// The leaf's answer, if it is the same everywhere.
    fn constant(&self) -> Option<bool> {
        None
    }

    /// One leaf true exactly where both are.
    fn and(&self, _other: &Self) -> Option<Self> {
        None
    }

    /// One leaf true exactly where either is.
    fn or(&self, _other: &Self) -> Option<Self> {
        None
    }

    /// What the leaf reads besides the entity it is asked about (DESIGN.md 5.8).
    fn deps(&self) -> CondDeps {
        CondDeps::empty()
    }
}

/// A filter as a tree over leaves of type `L`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Expr<L> {
    /// Always true, or never.
    Const(bool),
    Leaf(L),
    Not(Box<Expr<L>>),
    /// Every child holds; true when there are none.
    All(Box<[Expr<L>]>),
    /// Some child holds; false when there are none.
    Any(Box<[Expr<L>]>),
}

impl<L> Expr<L> {
    /// Evaluates the tree, asking `leaf` for each leaf it reaches, left to right, stopping as soon
    /// as the answer is known.
    pub fn eval<F: FnMut(&L) -> bool>(&self, leaf: &mut F) -> bool {
        match self {
            Self::Const(b) => *b,
            Self::Leaf(l) => leaf(l),
            Self::Not(x) => !x.eval(leaf),
            Self::All(xs) => xs.iter().all(|x| x.eval(leaf)),
            Self::Any(xs) => xs.iter().any(|x| x.eval(leaf)),
        }
    }

    /// The tree with each leaf replaced by the tree `f` gives for it.
    pub fn bind<M>(&self, f: &mut impl FnMut(&L) -> Expr<M>) -> Expr<M> {
        match self {
            Self::Const(b) => Expr::Const(*b),
            Self::Leaf(l) => f(l),
            Self::Not(x) => Expr::Not(Box::new(x.bind(f))),
            Self::All(xs) => Expr::All(xs.iter().map(|x| x.bind(f)).collect()),
            Self::Any(xs) => Expr::Any(xs.iter().map(|x| x.bind(f)).collect()),
        }
    }

    /// The tree with each leaf wrapped by `f`: a civilization filter as a unit's owner's.
    #[must_use]
    pub fn map<M>(&self, f: &mut impl FnMut(&L) -> M) -> Expr<M> {
        self.bind(&mut |l| Expr::Leaf(f(l)))
    }

    /// Every leaf, left to right.
    pub fn leaves(&self) -> Vec<&L> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(e) = stack.pop() {
            match e {
                Self::Const(_) => {}
                Self::Leaf(l) => out.push(l),
                Self::Not(x) => stack.push(x),
                Self::All(xs) | Self::Any(xs) => stack.extend(xs.iter().rev()),
            }
        }
        out
    }

    /// How deep the tree is: 1 for a leaf or a constant.
    #[must_use]
    pub fn depth(&self) -> usize {
        match self {
            Self::Const(_) | Self::Leaf(_) => 1,
            Self::Not(x) => 1 + x.depth(),
            Self::All(xs) | Self::Any(xs) => 1 + xs.iter().map(Self::depth).max().unwrap_or(0),
        }
    }

    /// The answer, if the tree has folded to a constant.
    #[must_use]
    pub fn constant(&self) -> Option<bool> {
        match self {
            Self::Const(b) => Some(*b),
            _ => None,
        }
    }
}

impl<L: Leaf> Expr<L> {
    /// What the tree's leaves read.
    #[must_use]
    pub fn deps(&self) -> CondDeps {
        self.leaves().into_iter().fold(CondDeps::empty(), |d, l| d | l.deps())
    }

    /// The tree simplified without changing its answer anywhere: constants propagate, `Not(Not
    /// x)` is `x`, nested `All`s and `Any`s flatten, leaves merge where [`Leaf`] says they can,
    /// equal children collapse, and a one-child `All` or `Any` is its child.
    #[must_use]
    pub fn fold(self) -> Self {
        match self {
            Self::Const(_) => self,
            Self::Leaf(l) => match l.constant() {
                Some(b) => Self::Const(b),
                None => Self::Leaf(l),
            },
            Self::Not(x) => match x.fold() {
                Self::Const(b) => Self::Const(!b),
                Self::Not(y) => *y,
                y => Self::Not(Box::new(y)),
            },
            Self::All(xs) => fold_list(xs, true),
            Self::Any(xs) => fold_list(xs, false),
        }
    }
}

/// Folds the children of an `All` (`all` true) or an `Any` (`all` false). `all`'s identity is
/// true and its absorbing value false; `Any`'s the other way round.
fn fold_list<L: Leaf>(xs: Box<[Expr<L>]>, all: bool) -> Expr<L> {
    let identity = all;
    let mut out: Vec<Expr<L>> = Vec::with_capacity(xs.len());
    // An explicit work list: flattening a nested list of the same kind pushes its children.
    let mut work: Vec<Expr<L>> = xs.into_vec();
    work.reverse();
    while let Some(x) = work.pop() {
        match x.fold() {
            Expr::Const(b) if b == identity => {}
            Expr::Const(_) => return Expr::Const(!identity),
            Expr::All(ys) if all => work.extend(ys.into_vec().into_iter().rev()),
            Expr::Any(ys) if !all => work.extend(ys.into_vec().into_iter().rev()),
            Expr::Leaf(l) => {
                let merged = out.iter().enumerate().find_map(|(i, o)| match o {
                    Expr::Leaf(m) => (if all { m.and(&l) } else { m.or(&l) }).map(|j| (i, j)),
                    _ => None,
                });
                match merged {
                    Some((i, joined)) => match joined.constant() {
                        // The merged leaf is the list's identity: it drops out.
                        Some(b) if b == identity => {
                            out.remove(i);
                        }
                        Some(_) => return Expr::Const(!identity),
                        None => out[i] = Expr::Leaf(joined),
                    },
                    None => {
                        let leaf = Expr::Leaf(l);
                        if !out.contains(&leaf) {
                            out.push(leaf);
                        }
                    }
                }
            }
            y => {
                if !out.contains(&y) {
                    out.push(y);
                }
            }
        }
    }
    match out.len() {
        0 => Expr::Const(identity),
        1 => out.pop().unwrap_or(Expr::Const(identity)),
        _ if all => Expr::All(out.into()),
        _ => Expr::Any(out.into()),
    }
}

impl Leaf for &str {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A leaf true in the worlds its mask names: merging is exact, as for one-valued facts.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Worlds(u16);

    impl Leaf for Worlds {
        fn constant(&self) -> Option<bool> {
            match self.0 {
                0 => Some(false),
                u16::MAX => Some(true),
                _ => None,
            }
        }

        fn and(&self, o: &Self) -> Option<Self> {
            Some(Self(self.0 & o.0))
        }

        fn or(&self, o: &Self) -> Option<Self> {
            Some(Self(self.0 | o.0))
        }
    }

    fn eval(e: &Expr<Worlds>, world: u32) -> bool {
        e.eval(&mut |w| w.0 & (1 << world) != 0)
    }

    fn l(mask: u16) -> Expr<Worlds> {
        Expr::Leaf(Worlds(mask))
    }

    #[test]
    fn constants_propagate() {
        let t = Expr::<Worlds>::Const(true);
        let f = Expr::<Worlds>::Const(false);
        assert_eq!(Expr::All([t.clone(), l(3)].into()).fold(), l(3));
        assert_eq!(Expr::All([f.clone(), l(3)].into()).fold(), f);
        assert_eq!(Expr::Any([t.clone(), l(3)].into()).fold(), t);
        assert_eq!(Expr::Any([f.clone(), l(3)].into()).fold(), l(3));
        assert_eq!(Expr::<Worlds>::All([].into()).fold(), t);
        assert_eq!(Expr::<Worlds>::Any([].into()).fold(), f);
        assert_eq!(Expr::Not(Box::new(Expr::Not(Box::new(l(5))))).fold(), l(5));
        assert_eq!(Expr::Not(Box::new(l(0))).fold(), t, "an empty leaf is false");
    }

    #[test]
    fn leaves_merge_and_lists_flatten() {
        let e = Expr::All([l(0b0111), Expr::All([l(0b0110), l(0b1110)].into())].into());
        assert_eq!(e.fold(), l(0b0110));
        let e = Expr::Any([l(0b0001), Expr::Not(Box::new(l(1))), l(0b0010)].into());
        assert_eq!(e.fold(), Expr::Any([l(0b0011), Expr::Not(Box::new(l(1)))].into()));
        assert_eq!(Expr::All([l(0b01), l(0b10)].into()).fold(), Expr::Const(false));
        assert_eq!(Expr::Any([l(0xff00), l(0x00ff)].into()).fold(), Expr::Const(true));
    }

    #[test]
    fn equal_children_collapse_and_depth_counts() {
        let n = Expr::Not(Box::new(Expr::Leaf("a")));
        let e: Expr<&str> = Expr::Any([n.clone(), n.clone(), Expr::Leaf("b")].into());
        assert_eq!(e.clone().fold(), Expr::Any([n, Expr::Leaf("b")].into()));
        assert_eq!(e.depth(), 3);
        assert_eq!(e.leaves(), [&"a", &"a", &"b"]);
        for w in 0..16 {
            let x = Expr::Any([l(0b1010), Expr::Not(Box::new(l(0b0110)))].into());
            assert_eq!(eval(&x.clone().fold(), w), eval(&x, w));
        }
    }
}
