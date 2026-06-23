//! A generational arena for Janus's hot trees (DOM, box tree, fragment tree).
//!
//! Nodes live in a flat `Vec` and are referenced by a small `Copy` [`Index`]
//! (slot + generation) instead of `Rc<RefCell<…>>`. This gives cache-friendly,
//! pointer-chase-free storage — the data-oriented backbone the engine's speed
//! pillar mandates — and use-after-free safety: when a slot is freed its
//! generation is bumped, so a stale [`Index`] fails its generation check and
//! [`Arena::get`] returns `None` rather than aliasing a reused node.
//!
//! All operations are deterministic, which matters: golden-image tests and
//! reproducible agent snapshots depend on identical structure regardless of
//! allocation history.
//!
//! ```
//! use janus_arena::Arena;
//! let mut a = Arena::new();
//! let id = a.insert("root");
//! assert_eq!(a.get(id), Some(&"root"));
//! assert_eq!(a.remove(id), Some("root"));
//! assert_eq!(a.get(id), None); // stale index is rejected
//! ```

/// A stable handle into an [`Arena`]: a slot plus the generation that occupied
/// it. `Copy` and 8 bytes, so it is cheap to store and pass by value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Index {
    slot: u32,
    generation: u32,
}

impl Index {
    /// The slot this index points at.
    #[inline]
    #[must_use]
    pub fn slot(self) -> u32 {
        self.slot
    }

    /// The generation this index was minted with.
    #[inline]
    #[must_use]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Debug)]
enum Entry<T> {
    Occupied {
        generation: u32,
        value: T,
    },
    Vacant {
        generation: u32,
        next_free: Option<u32>,
    },
}

/// A generational arena of `T`.
#[derive(Debug)]
pub struct Arena<T> {
    entries: Vec<Entry<T>>,
    free_head: Option<u32>,
    len: usize,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            free_head: None,
            len: 0,
        }
    }
}

impl<T> Arena<T> {
    /// Create an empty arena.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty arena with room for `cap` entries before reallocating.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cap),
            free_head: None,
            len: 0,
        }
    }

    /// Number of live entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the arena holds no live entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Insert `value`, returning a fresh [`Index`]. Reuses a freed slot when
    /// one is available (with a bumped generation), otherwise appends.
    pub fn insert(&mut self, value: T) -> Index {
        if let Some(slot) = self.free_head {
            // Read the vacant slot's fields before mutating `self` elsewhere.
            let (generation, next_free) = match &self.entries[slot as usize] {
                Entry::Vacant {
                    generation,
                    next_free,
                } => (*generation, *next_free),
                Entry::Occupied { .. } => unreachable!("free list pointed at an occupied slot"),
            };
            self.free_head = next_free;
            self.entries[slot as usize] = Entry::Occupied { generation, value };
            self.len += 1;
            Index { slot, generation }
        } else {
            let slot =
                u32::try_from(self.entries.len()).expect("janus-arena: slot space exhausted");
            self.entries.push(Entry::Occupied {
                generation: 0,
                value,
            });
            self.len += 1;
            Index {
                slot,
                generation: 0,
            }
        }
    }

    /// Borrow the value at `index`, or `None` if it was removed (stale index).
    #[must_use]
    pub fn get(&self, index: Index) -> Option<&T> {
        match self.entries.get(index.slot as usize) {
            Some(Entry::Occupied { generation, value }) if *generation == index.generation => {
                Some(value)
            }
            _ => None,
        }
    }

    /// Mutably borrow the value at `index`, or `None` if it is stale.
    #[must_use]
    pub fn get_mut(&mut self, index: Index) -> Option<&mut T> {
        match self.entries.get_mut(index.slot as usize) {
            Some(Entry::Occupied { generation, value }) if *generation == index.generation => {
                Some(value)
            }
            _ => None,
        }
    }

    /// Whether `index` still refers to a live entry.
    #[must_use]
    pub fn contains(&self, index: Index) -> bool {
        self.get(index).is_some()
    }

    /// Remove and return the value at `index`, freeing its slot for reuse.
    /// Returns `None` if the index is already stale.
    pub fn remove(&mut self, index: Index) -> Option<T> {
        let slot = index.slot as usize;
        let live = matches!(
            self.entries.get(slot),
            Some(Entry::Occupied { generation, .. }) if *generation == index.generation
        );
        if !live {
            return None;
        }
        let next_generation = index.generation.wrapping_add(1);
        let old = std::mem::replace(
            &mut self.entries[slot],
            Entry::Vacant {
                generation: next_generation,
                next_free: self.free_head,
            },
        );
        self.free_head = Some(index.slot);
        self.len -= 1;
        match old {
            Entry::Occupied { value, .. } => Some(value),
            Entry::Vacant { .. } => None,
        }
    }

    /// Iterate over live `(Index, &T)` pairs, in slot order (deterministic).
    pub fn iter(&self) -> impl Iterator<Item = (Index, &T)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| match entry {
                Entry::Occupied { generation, value } => Some((
                    Index {
                        slot: slot as u32,
                        generation: *generation,
                    },
                    value,
                )),
                Entry::Vacant { .. } => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut a = Arena::new();
        let x = a.insert(10);
        let y = a.insert(20);
        assert_eq!(a.get(x), Some(&10));
        assert_eq!(a.get(y), Some(&20));
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn get_mut_mutates() {
        let mut a = Arena::new();
        let x = a.insert(1);
        *a.get_mut(x).unwrap() += 41;
        assert_eq!(a.get(x), Some(&42));
    }

    #[test]
    fn remove_returns_value_and_invalidates() {
        let mut a = Arena::new();
        let x = a.insert("v");
        assert_eq!(a.remove(x), Some("v"));
        assert_eq!(a.remove(x), None);
        assert_eq!(a.get(x), None);
        assert!(!a.contains(x));
        assert_eq!(a.len(), 0);
    }

    #[test]
    fn freed_slot_is_reused_with_new_generation() {
        let mut a = Arena::new();
        let x = a.insert("first");
        a.remove(x);
        let y = a.insert("second");
        // Same slot, but a stale handle to it must not resolve.
        assert_eq!(x.slot(), y.slot());
        assert_ne!(x.generation(), y.generation());
        assert_eq!(a.get(x), None);
        assert_eq!(a.get(y), Some(&"second"));
    }

    #[test]
    fn iter_yields_live_entries_in_slot_order() {
        let mut a = Arena::new();
        let x = a.insert(1);
        let _y = a.insert(2);
        let z = a.insert(3);
        a.remove(x); // free slot 0
        let collected: Vec<_> = a.iter().map(|(_, &v)| v).collect();
        assert_eq!(collected, vec![2, 3]);
        assert_eq!(a.get(z), Some(&3));
    }
}
