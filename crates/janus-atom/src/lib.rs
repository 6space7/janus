//! String interning for Janus.
//!
//! Hot browser data — element tag names, attribute names, class names, CSS
//! property names — is compared constantly. Interning maps each distinct
//! string to a small integer [`Atom`] so equality and hashing are `O(1)`
//! integer operations and storage is deduplicated. This is one of the
//! "hot-data conventions" mandated by the engine's speed pillar.
//!
//! [`Interner`] is explicit, not a global: an [`Atom`] is only meaningful with
//! respect to the [`Interner`] that produced it. A sharded/global interner can
//! be layered on later without changing this API.
//!
//! ```
//! use janus_atom::Interner;
//! let mut i = Interner::new();
//! let div = i.intern("div");
//! assert_eq!(div, i.intern("div"));   // idempotent
//! assert_ne!(div, i.intern("span"));  // distinct strings → distinct atoms
//! assert_eq!(i.resolve(div), "div");  // round-trips
//! ```

use std::sync::Arc;

use rustc_hash::FxHashMap;

/// An interned string, represented as a 32-bit id into an [`Interner`].
///
/// Equality and hashing are `O(1)` integer operations. An `Atom` is only valid
/// for the [`Interner`] that created it; resolving it elsewhere is a logic
/// error (and will panic or return the wrong string).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Atom(u32);

impl Atom {
    /// The raw id backing this atom. Stable for the life of its interner.
    #[inline]
    #[must_use]
    pub fn id(self) -> u32 {
        self.0
    }
}

/// An append-only string interner.
///
/// Strings are never removed, so [`Atom`] ids stay valid for the interner's
/// whole lifetime. Backing storage is shared via [`Arc<str>`], so interning
/// does not duplicate the string bytes.
#[derive(Default, Debug)]
pub struct Interner {
    map: FxHashMap<Arc<str>, Atom>,
    strings: Vec<Arc<str>>,
}

impl Interner {
    /// Create an empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty interner with room for `cap` distinct strings.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: FxHashMap::with_capacity_and_hasher(cap, rustc_hash::FxBuildHasher),
            strings: Vec::with_capacity(cap),
        }
    }

    /// Intern `s`, returning its [`Atom`]. Idempotent: equal strings always
    /// return the same atom.
    pub fn intern(&mut self, s: &str) -> Atom {
        if let Some(&atom) = self.map.get(s) {
            return atom;
        }
        let atom =
            Atom(u32::try_from(self.strings.len()).expect("janus-atom: atom space exhausted"));
        let shared: Arc<str> = Arc::from(s);
        self.strings.push(Arc::clone(&shared));
        self.map.insert(shared, atom);
        atom
    }

    /// Look up the atom for `s` without interning it.
    #[must_use]
    pub fn get(&self, s: &str) -> Option<Atom> {
        self.map.get(s).copied()
    }

    /// Resolve an [`Atom`] back to its string slice.
    ///
    /// # Panics
    /// Panics if `atom` did not come from this interner.
    #[inline]
    #[must_use]
    pub fn resolve(&self, atom: Atom) -> &str {
        &self.strings[atom.0 as usize]
    }

    /// Number of distinct interned strings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Whether no strings have been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_is_idempotent() {
        let mut i = Interner::new();
        let a = i.intern("hello");
        let b = i.intern("hello");
        assert_eq!(a, b);
        assert_eq!(i.len(), 1);
    }

    #[test]
    fn distinct_strings_get_distinct_atoms() {
        let mut i = Interner::new();
        let a = i.intern("a");
        let b = i.intern("b");
        assert_ne!(a, b);
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn resolve_round_trips() {
        let mut i = Interner::new();
        let atoms: Vec<_> = ["div", "span", "a", "div"]
            .iter()
            .map(|s| i.intern(s))
            .collect();
        assert_eq!(i.resolve(atoms[0]), "div");
        assert_eq!(i.resolve(atoms[1]), "span");
        assert_eq!(i.resolve(atoms[2]), "a");
        assert_eq!(atoms[0], atoms[3]);
    }

    #[test]
    fn get_does_not_intern() {
        let mut i = Interner::new();
        assert_eq!(i.get("nope"), None);
        let a = i.intern("yes");
        assert_eq!(i.get("yes"), Some(a));
        assert_eq!(i.get("nope"), None);
    }

    #[test]
    fn empty_state() {
        let i = Interner::new();
        assert!(i.is_empty());
        assert_eq!(i.len(), 0);
    }
}
