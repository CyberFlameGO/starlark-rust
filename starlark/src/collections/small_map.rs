/*
 * Copyright 2019 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! A Map with deterministic iteration order that specializes its storage based on the number of
//! entries to optimize memory. This is essentially `IndexMap` with two changes:
//! * no index is created for small maps
//! * short hashes are stored next to keys

use std::{
    cmp::Ordering,
    fmt::{self, Debug},
    hash::{Hash, Hasher},
    iter::FromIterator,
    mem,
};

use gazebo::{
    coerce::{Coerce, CoerceKey},
    prelude::*,
};
use hashbrown::raw::RawTable;
use indexmap::Equivalent;

use crate::collections::{
    hash::{BorrowHashed, Hashed},
    vec_map::{Bucket, VMIntoIter, VMIter, VMIterMut, VecMap},
    StarlarkHasher,
};

/// Max size of map when we do not create index.
// TODO: benchmark, is this the right threshold
const NO_INDEX_THRESHOLD: usize = 12;

pub type MHIter<'a, K, V> = VMIter<'a, K, V>;

pub type MHIterMut<'a, K, V> = VMIterMut<'a, K, V>;

pub type MHIntoIter<K, V> = VMIntoIter<K, V>;

/// An memory-efficient key-value map with determinstic order.
///
/// Provides the standard container operations, modelled most closely on [`IndexMap`](indexmap::IndexMap), plus:
///
/// * Variants which take an already hashed value, e.g. [`get_hashed`](SmallMap::get_hashed).
///
/// * Functions which work with the position, e.g. [`get_index_of`](SmallMap::get_index_of).
#[repr(C)]
#[derive(Clone, Default_)]
pub struct SmallMap<K, V> {
    entries: VecMap<K, V>,
    /// Map a key to the index in `entries`.
    /// This field is initialized when the size of the map exceeds `NO_INDEX_THRESHOLD`.
    index: Option<Box<RawTable<usize>>>,
}

unsafe impl<FromK, FromV, ToK, ToV> Coerce<SmallMap<ToK, ToV>> for SmallMap<FromK, FromV>
where
    FromK: CoerceKey<ToK>,
    FromV: Coerce<ToV>,
{
}

impl<K: Debug, V: Debug> Debug for SmallMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K, V> SmallMap<K, V> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            entries: VecMap::new(),
            index: None,
        }
    }

    #[inline]
    pub fn with_capacity(n: usize) -> Self {
        if n <= NO_INDEX_THRESHOLD {
            SmallMap {
                entries: VecMap::with_capacity(n),
                index: None,
            }
        } else {
            SmallMap {
                entries: VecMap::with_capacity(n),
                index: Some(box RawTable::with_capacity(n)),
            }
        }
    }

    /// Create with largest capacity which is represented by `Vec`.
    #[inline]
    pub(crate) fn with_capacity_largest_vec() -> Self {
        Self::with_capacity(NO_INDEX_THRESHOLD)
    }

    /// Drop the index if the map is too small, and the index is not really needed.
    ///
    /// We don't allocate index prematurely when we add entries the map,
    /// but we keep it allocated when we remove entries from the map.
    ///
    /// This function allows to reclaim memory after some entries are removed.
    pub(crate) fn maybe_drop_index(&mut self) {
        if self.entries.len() <= NO_INDEX_THRESHOLD {
            self.index = None;
        }
    }

    #[inline]
    pub(crate) fn into_raw_parts(self) -> (VecMap<K, V>, Option<Box<RawTable<usize>>>) {
        (self.entries, self.index)
    }

    #[inline]
    pub(crate) unsafe fn from_raw_parts(
        entries: VecMap<K, V>,
        index: Option<Box<RawTable<usize>>>,
    ) -> SmallMap<K, V> {
        if let Some(index) = &index {
            // Quick smoke test.
            // We don't validate indices are correct hence this function is unsafe.
            assert!(entries.len() == index.len());
        } else {
            assert!(entries.len() <= NO_INDEX_THRESHOLD);
        }
        SmallMap { entries, index }
    }

    #[inline]
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &K> + Clone {
        self.entries.keys()
    }

    #[inline]
    pub fn values(&self) -> impl ExactSizeIterator<Item = &V> + Clone {
        self.entries.values()
    }

    #[inline]
    pub fn values_mut(&mut self) -> impl ExactSizeIterator<Item = &mut V> {
        self.entries.values_mut()
    }

    #[inline]
    pub fn iter(&self) -> MHIter<'_, K, V> {
        self.entries.iter()
    }

    #[inline]
    pub fn iter_hashed(&self) -> impl ExactSizeIterator<Item = (BorrowHashed<K>, &V)> {
        self.entries.iter_hashed()
    }

    #[inline]
    pub fn into_iter_hashed(self) -> impl ExactSizeIterator<Item = (Hashed<K>, V)> {
        self.entries.into_iter_hashed()
    }

    #[inline]
    pub fn iter_mut(&mut self) -> MHIterMut<'_, K, V> {
        self.entries.iter_mut()
    }

    #[inline]
    pub fn into_iter(self) -> MHIntoIter<K, V> {
        self.entries.into_iter()
    }

    #[inline]
    pub fn get_hashed<Q>(&self, key: BorrowHashed<Q>) -> Option<&V>
    where
        Q: Equivalent<K> + ?Sized,
        K: Eq,
    {
        self.get_index_of_hashed(key)
            .map(|index| unsafe { &self.entries.get_unchecked(index).value })
    }

    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
        K: Eq,
    {
        self.get_hashed(BorrowHashed::new(key))
    }

    #[inline]
    pub fn get_full<Q>(&self, key: &Q) -> Option<(usize, &K, &V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
        K: Eq,
    {
        self.get_index_of_hashed(BorrowHashed::new(key))
            .map(|index| {
                let Bucket { key, value, .. } = unsafe { self.entries.get_unchecked(index) };
                (index, key, value)
            })
    }

    #[inline]
    pub fn get_index_of_hashed<Q>(&self, key: BorrowHashed<Q>) -> Option<usize>
    where
        Q: Equivalent<K> + ?Sized,
        K: Eq,
    {
        match &self.index {
            None => self.entries.get_index_of_hashed(key),
            Some(index) => index
                .get(key.hash().promote(), |&index| unsafe {
                    key.key().equivalent(&self.entries.get_unchecked(index).key)
                })
                .copied(),
        }
    }

    #[inline]
    pub fn get_index(&self, index: usize) -> Option<(&K, &V)> {
        self.entries.get_index(index)
    }

    #[inline]
    pub fn get_index_of<Q>(&self, key: &Q) -> Option<usize>
    where
        Q: Hash + Equivalent<K> + ?Sized,
        K: Eq,
    {
        self.get_index_of_hashed(BorrowHashed::new(key))
    }

    #[inline]
    pub fn get_mut_hashed<Q>(&mut self, key: BorrowHashed<Q>) -> Option<&mut V>
    where
        Q: Equivalent<K> + ?Sized,
        K: Eq,
    {
        let i = self.get_index_of_hashed(key)?;
        debug_assert!(i < self.entries.buckets.len());
        Some(unsafe { &mut self.entries.buckets.get_unchecked_mut(i).value })
    }

    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
        K: Eq,
    {
        self.get_mut_hashed(BorrowHashed::new(key))
    }

    #[inline]
    pub fn contains_key_hashed<Q>(&self, key: BorrowHashed<Q>) -> bool
    where
        Q: Equivalent<K> + ?Sized,
        K: Eq,
    {
        self.get_index_of_hashed(key).is_some()
    }

    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        Q: Hash + Equivalent<K> + ?Sized,
        K: Eq,
    {
        self.contains_key_hashed(BorrowHashed::new(key))
    }

    #[inline]
    pub fn reserve(&mut self, additional: usize)
    where
        K: Eq,
    {
        self.entries.reserve(additional);
        if let Some(index) = &mut self.index {
            index.reserve(additional, Self::hasher(&self.entries));
        } else if self.len() + additional > NO_INDEX_THRESHOLD {
            self.create_index(self.len() + additional);
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// Give a best guess as to how much heap memory is being used.
    /// Used internally, but not exported as this isn't a usual API.
    pub(crate) fn extra_memory(&self) -> usize {
        self.entries.extra_memory()
            + match &self.index {
                None => 0,
                Some(index) => {
                    // We estimate the size of the hashtable (for this we just use hashbrown's code), and it
                    // contains usizes as well (the indices) so that goes in too. Finally, there are
                    // control bytes for each of the buckets. There is one control byte per entry, but
                    // also an implementation-dependent extra padding. When SSE2 is enabled, that's 16
                    // bytes.

                    index.buckets() * (mem::size_of::<usize>() + 1) + 16
                }
            }
    }

    #[cold]
    fn create_index(&mut self, capacity: usize) {
        debug_assert!(self.index.is_none());
        debug_assert!(capacity >= self.entries.len());
        let mut index = RawTable::with_capacity(capacity);
        for (i, b) in self.entries.buckets.iter().enumerate() {
            index.insert_no_grow(b.hash.promote(), i);
        }
        self.index = Some(box index);
    }

    /// Hasher for index resize.
    #[inline(always)]
    fn hasher<'a>(entries: &'a VecMap<K, V>) -> impl Fn(&usize) -> u64 + 'a {
        move |&index| {
            debug_assert!(index < entries.len());
            unsafe { entries.buckets.get_unchecked(index).hash.promote() }
        }
    }

    #[inline]
    fn insert_unique_unchecked(&mut self, key: Hashed<K>, val: V) {
        let hash = key.hash();
        let entry_index = self.entries.len();
        self.entries.insert_unique_unchecked(key, val);
        if let Some(index) = &mut self.index {
            index.insert(hash.promote(), entry_index, Self::hasher(&self.entries));
        } else if self.entries.len() == NO_INDEX_THRESHOLD + 1 {
            self.create_index(self.entries.len());
        } else {
            debug_assert!(self.entries.len() < NO_INDEX_THRESHOLD + 1);
        }
    }

    #[inline]
    pub fn insert_hashed(&mut self, key: Hashed<K>, val: V) -> Option<V>
    where
        K: Eq,
    {
        match self.get_index_of_hashed(key.borrow()) {
            None => {
                self.insert_unique_unchecked(key, val);
                None
            }
            Some(i) => unsafe {
                debug_assert!(i < self.entries.len());
                Some(mem::replace(
                    &mut self.entries.buckets.get_unchecked_mut(i).value,
                    val,
                ))
            },
        }
    }

    #[inline]
    pub fn insert(&mut self, key: K, val: V) -> Option<V>
    where
        K: Hash + Eq,
    {
        self.insert_hashed(Hashed::new(key), val)
    }

    pub fn remove_hashed<Q>(&mut self, key: BorrowHashed<Q>) -> Option<V>
    where
        Q: ?Sized + Equivalent<K>,
        K: Eq,
    {
        self.remove_hashed_entry(key).map(|(_k, v)| v)
    }

    pub fn remove_hashed_entry<Q>(&mut self, key: BorrowHashed<Q>) -> Option<(K, V)>
    where
        Q: ?Sized + Equivalent<K>,
        K: Eq,
    {
        let hash = key.hash();
        if let Some(index) = &mut self.index {
            let entries = &self.entries;
            let i = index.remove_entry(hash.promote(), |&i| unsafe {
                key.key().equivalent(&entries.get_unchecked(i).key)
            })?;
            unsafe {
                // This updates all the table, which is `O(N)`,
                // but this is very inefficient when the map is large
                // and only last elements are removed (could be `O(1)`).
                // Practically this is not an issue, we do not remove elements often, but
                // TODO(nga): fix that.
                for bucket in index.iter() {
                    if *bucket.as_mut() >= i {
                        *bucket.as_mut() -= 1;
                    }
                }
            }
            let Bucket { key, value, .. } = self.entries.buckets.remove(i);
            Some((key, value))
        } else {
            self.entries.remove_hashed_entry(key)
        }
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        Q: ?Sized + Hash + Equivalent<K>,
        K: Eq,
    {
        self.remove_hashed(BorrowHashed::new(key))
    }

    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        Q: ?Sized + Hash + Equivalent<K>,
        K: Eq,
    {
        self.remove_hashed_entry(BorrowHashed::new(key))
    }

    #[inline]
    pub fn entry_hashed(&mut self, key: Hashed<K>) -> Entry<'_, K, V>
    where
        K: Eq,
    {
        match self.get_index_of_hashed(key.borrow()) {
            Some(i) => {
                let entry = unsafe { self.entries.get_unchecked_mut(i) };
                Entry::Occupied(OccupiedEntry {
                    key: &entry.key,
                    value: &mut entry.value,
                })
            }
            None => Entry::Vacant(VacantEntry { key, map: self }),
        }
    }

    /// Remove the last element.
    pub fn pop(&mut self) -> Option<(K, V)>
    where
        K: Eq,
    {
        match self.entries.buckets.pop() {
            None => None,
            Some(Bucket { key, value, hash }) => {
                if let Some(index) = &mut self.index {
                    let removed = index.remove_entry(hash.promote(), |&i| i == self.entries.len());
                    debug_assert!(removed.unwrap() == self.entries.len());
                }
                Some((key, value))
            }
        }
    }

    #[inline]
    pub fn entry(&mut self, key: K) -> Entry<'_, K, V>
    where
        K: Eq + Hash,
    {
        self.entry_hashed(Hashed::new(key))
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
        if let Some(index) = &mut self.index {
            // Note we are keeping the `index` object initialized here.
            // So next insert will have to update the index.
            // Which is probably suboptimal (hard to say),
            // but `clear` is rare operation anyway.
            index.clear();
        }
    }

    /// Basic check the map invariants are hold.
    #[cfg(test)]
    fn state_check(&self) {
        if let Some(index) = &self.index {
            assert_eq!(self.entries.len(), index.len());
            let mut set_fields = vec![false; self.entries.len()];
            unsafe {
                for bucket in index.iter() {
                    let i = *bucket.as_ref();
                    let prev = mem::replace(&mut set_fields[i], true);
                    assert!(!prev);
                }
            }
        } else {
            assert!(self.entries.len() <= NO_INDEX_THRESHOLD);
        }
    }
}

pub struct OccupiedEntry<'a, K, V> {
    key: &'a K,
    value: &'a mut V,
}

pub struct VacantEntry<'a, K, V> {
    key: Hashed<K>,
    map: &'a mut SmallMap<K, V>,
}

pub enum Entry<'a, K, V> {
    Occupied(OccupiedEntry<'a, K, V>),
    Vacant(VacantEntry<'a, K, V>),
}

impl<'a, K, V> OccupiedEntry<'a, K, V> {
    #[inline]
    pub fn key(&self) -> &K {
        self.key
    }

    #[inline]
    pub fn get(&self) -> &V {
        self.value
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut V {
        self.value
    }
}

impl<'a, K, V> VacantEntry<'a, K, V>
where
    K: Eq,
{
    #[inline]
    pub fn key(&self) -> &K {
        self.key.key()
    }

    // NOTE(nga): `VacantEntry::insert` is supposed to return `&'a mut V`
    #[inline]
    pub fn insert(self, value: V) {
        self.map.insert_unique_unchecked(self.key, value);
    }
}

impl<'a, K, V> Entry<'a, K, V>
where
    K: Eq,
{
    #[inline]
    pub fn key(&self) -> &K {
        match self {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => e.key(),
        }
    }
}

impl<K, V> FromIterator<(K, V)> for SmallMap<K, V>
where
    K: Hash + Eq,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut mp = Self::with_capacity(iter.size_hint().0);
        for (k, v) in iter {
            mp.insert(k, v);
        }
        mp
    }
}

impl<K, V> FromIterator<(Hashed<K>, V)> for SmallMap<K, V>
where
    K: Eq,
{
    fn from_iter<I: IntoIterator<Item = (Hashed<K>, V)>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut mp = Self::with_capacity(iter.size_hint().0);
        for (k, v) in iter {
            mp.insert_hashed(k, v);
        }
        mp
    }
}

impl<K, V> IntoIterator for SmallMap<K, V> {
    type Item = (K, V);
    type IntoIter = MHIntoIter<K, V>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a SmallMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = MHIter<'a, K, V>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut SmallMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = MHIterMut<'a, K, V>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K: Eq, V: PartialEq> PartialEq for SmallMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter_hashed()
                .all(|(k, v)| other.get_hashed(k) == Some(v))
    }
}

impl<K: Eq, V: Eq> Eq for SmallMap<K, V> {}

impl<K: Hash, V: Hash> Hash for SmallMap<K, V> {
    /// The hash of a map is the sum of hashes of all its elements, so that we guarantee equal hash
    /// means equals
    fn hash<H: Hasher>(&self, state: &mut H) {
        // we could use 'iter_hashed' here, but then we'd be hashing hashes of keys instead of the
        // keys itself, which is a little less correct and flexible.
        self.iter()
            .map(|e| {
                let mut s = StarlarkHasher::new();
                e.hash(&mut s);
                std::num::Wrapping(s.finish())
            })
            .sum::<std::num::Wrapping<u64>>()
            .hash(state)
    }
}

impl<K: PartialOrd + Eq, V: PartialOrd> PartialOrd for SmallMap<K, V> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.iter().partial_cmp(other.iter())
    }
}

impl<K: Ord, V: Ord> Ord for SmallMap<K, V> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.iter().cmp(other.iter())
    }
}

/// Create a [`SmallMap`](SmallMap) from a list of key-value pairs.
///
/// ## Example
///
/// ```
/// #[macro_use] extern crate starlark;
/// # fn main() {
///
/// let map = smallmap!{
///     "a" => 1,
///     "b" => 2,
/// };
/// assert_eq!(map.get("a"), Some(&1));
/// assert_eq!(map.get("b"), Some(&2));
/// assert_eq!(map.get("c"), None);
/// # }
/// ```
#[macro_export]
macro_rules! smallmap {
    (@single $($x:tt)*) => (());
    (@count $($rest:expr),*) => (<[()]>::len(&[$(smallmap!(@single $rest)),*]));

    ($($key:expr => $value:expr,)+) => { smallmap!($($key => $value),+) };
    ($($key:expr => $value:expr),*) => {
        {
            let cap = smallmap!(@count $($key),*);
            let mut map = $crate::collections::SmallMap::with_capacity(cap);
            $(
                map.insert($key, $value);
            )*
            map
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map() {
        let m = SmallMap::<i8, &str>::new();
        assert_eq!(m.is_empty(), true);
        assert_eq!(m.len(), 0);
        assert_eq!(m.iter().next(), None);
    }

    #[test]
    fn few_entries() {
        let entries1 = vec![(0, 'a'), (1, 'b')];
        let m1 = entries1.iter().copied().collect::<SmallMap<_, _>>();

        let entries2 = vec![(1, 'b'), (0, 'a')];
        let m2 = entries2.iter().copied().collect::<SmallMap<_, _>>();
        assert_eq!(m1.is_empty(), false);
        assert_eq!(m1.len(), 2);
        assert_eq!(m2.is_empty(), false);
        assert_eq!(m2.len(), 2);

        assert_eq!(m1.iter().eq(entries1.iter().map(|(k, v)| (k, v))), true);
        assert_eq!(m2.iter().eq(entries2.iter().map(|(k, v)| (k, v))), true);
        assert_eq!(m1.iter().eq(m2.iter()), false);
        assert_eq!(m1.eq(&m1), true);
        assert_eq!(m2.eq(&m2), true);
        assert_eq!(m1, m2);

        assert_eq!(m1.get(&0), Some(&'a'));
        assert_eq!(m1.get(&3), None);
        assert_eq!(m2.get(&1), Some(&'b'));
        assert_eq!(m2.get(&3), None);

        assert_eq!(m1.get_index(0), Some((&0, &'a')));
        assert_eq!(m1.get_index(1), Some((&1, &'b')));
        assert_eq!(m1.get_index(2), None);

        assert_ne!(m1, smallmap! { 0 => 'a', 1 => 'c' });

        let iter = m1.iter();
        let (values1, values2): (Vec<_>, Vec<_>) = (iter.clone().collect(), iter.collect());
        assert_eq!(values1, values2);
    }

    #[test]
    fn many_entries() {
        let numbers = 0..26;
        let letters = 'a'..='z';

        let entries1 = numbers.zip(letters);
        let m1 = entries1.clone().collect::<SmallMap<_, _>>();

        let numbers = (0..26).rev();
        let letters = ('a'..='z').rev();
        let entries2 = numbers.zip(letters);
        let m2 = entries2.clone().collect::<SmallMap<_, _>>();
        assert_eq!(m1.is_empty(), false);
        assert_eq!(m1.len(), 26);
        assert_eq!(m2.is_empty(), false);
        assert_eq!(m2.len(), 26);

        assert_eq!(m1.clone().into_iter().eq(entries1), true);
        assert_eq!(m2.clone().into_iter().eq(entries2), true);
        assert_eq!(m1.iter().eq(m2.iter()), false);
        assert_eq!(m1.eq(&m1), true);
        assert_eq!(m2.eq(&m2), true);
        assert_eq!(m1, m2);

        assert_eq!(m1.get(&1), Some(&'b'));
        assert_eq!(m1.get(&30), None);
        assert_eq!(m2.get(&0), Some(&'a'));
        assert_eq!(m2.get(&30), None);
        assert_eq!(m2.get_full(&0), Some((25, &0, &'a')));
        assert_eq!(m2.get_full(&25), Some((0, &25, &'z')));
        assert_eq!(m2.get_full(&29), None);

        let not_m1 = {
            let mut m = m1.clone();
            m.remove(&1);
            m
        };
        assert_ne!(m1, not_m1);

        let iter = m1.iter();
        let (values1, values2): (Vec<_>, Vec<_>) = (iter.clone().collect(), iter.collect());
        assert_eq!(values1, values2);
    }

    #[test]
    fn test_smallmap_macro() {
        let map = smallmap![1 => "a", 3 => "b"];
        let mut i = map.into_iter();
        assert_eq!(i.next(), Some((1, "a")));
        assert_eq!(i.next(), Some((3, "b")));
        assert_eq!(i.next(), None);
    }

    #[test]
    fn test_clone() {
        let map = smallmap![1 => "a", 3 => "b"];
        let iter = map.iter();
        let values1: Vec<_> = iter.clone().collect();
        let values2: Vec<_> = iter.collect();
        assert_eq!(vec![(&1, &"a"), (&3, &"b")], values1);
        assert_eq!(values1, values2);

        let iter = map.keys();
        let values1: Vec<_> = iter.clone().collect();
        let values2: Vec<_> = iter.collect();
        assert_eq!(vec![&1, &3], values1);
        assert_eq!(values1, values2);

        let iter = map.values();
        let values1: Vec<_> = iter.clone().collect();
        let values2: Vec<_> = iter.collect();
        assert_eq!(vec![&"a", &"b"], values1);
        assert_eq!(values1, values2);
    }

    #[test]
    fn test_duplicate_hashes() {
        // A type which always gives hash collisions
        #[derive(PartialEq, Eq, Debug)]
        struct K(i32);
        #[allow(clippy::derive_hash_xor_eq)]
        impl Hash for K {
            fn hash<H: Hasher>(&self, _state: &mut H) {}
        }

        let mut map = smallmap![K(1) => "test", K(3) => "more"];
        assert_eq!(map.get(&K(1)), Some(&"test"));
        assert_eq!(map.get(&K(2)), None);
        assert_eq!(map.get(&K(3)), Some(&"more"));

        assert_eq!(map.insert(K(2), "magic"), None);
        assert_eq!(map.get(&K(2)), Some(&"magic"));

        assert_eq!(map.remove(&K(1)), Some("test"));
        assert_eq!(map.get(&K(1)), None);
        assert_eq!(map.keys().collect::<Vec<_>>(), vec![&K(3), &K(2)]);
    }

    #[test]
    fn test_smallmap_debug() {
        let s = format!("{:?}", smallmap![1 => "test", 2 => "more"]);
        assert_eq!(s, "{1: \"test\", 2: \"more\"}")
    }

    #[test]
    fn entry() {
        let mut map = SmallMap::new();
        for i in 0..100 {
            match map.entry(i) {
                Entry::Vacant(e) => e.insert(i * 2),
                Entry::Occupied(..) => panic!(),
            }
            match map.entry(i) {
                Entry::Occupied(..) => {}
                Entry::Vacant(..) => panic!(),
            }
        }
    }

    #[test]
    fn test_pop_small() {
        let mut map = SmallMap::new();
        for i in 0..=5 {
            map.insert(i, i * 10);
        }
        for i in (0..=5).rev() {
            assert_eq!((i, i * 10), map.pop().unwrap());
            map.state_check();
        }
        assert!(map.is_empty());
    }

    #[test]
    fn test_pop_large() {
        let mut map = SmallMap::new();
        for i in 0..=500 {
            map.insert(i, i * 10);
        }
        for i in (0..=500).rev() {
            assert_eq!((i, i * 10), map.pop().unwrap());
            if i % 100 == 0 {
                map.state_check();
            }
        }
        assert!(map.is_empty());
    }
}
