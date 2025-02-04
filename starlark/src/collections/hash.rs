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

use std::{
    hash::{Hash, Hasher},
    ops::Deref,
};

use gazebo::{coerce::Coerce, prelude::*};
use indexmap::Equivalent;

use crate as starlark;
use crate::collections::{idhasher::mix_u32, StarlarkHasher};

/// A hash value.
///
/// Contained value must be compatible with a value produced by `StarlarkHasher`
#[derive(Hash, Eq, PartialEq, Clone, Copy, Dupe, Debug, Default, Trace)]
// Hash value must be well swizzled.
pub struct StarlarkHashValue(u32);

/// A key and its hash.
#[derive(PartialEq, Eq, Debug, Clone, Copy, Trace, Coerce)]
#[repr(C)]
pub struct Hashed<K> {
    hash: StarlarkHashValue,
    key: K,
}

/// A borrowed key and its hash.
#[derive(Copy_, Clone_, Dupe_)]
pub struct BorrowHashed<'a, Q: ?Sized> {
    hash: StarlarkHashValue,
    key: &'a Q,
}

impl StarlarkHashValue {
    /// Create a new [`StarlarkHashValue`] using the [`Hash`] trait
    /// for given key.
    pub fn new<K: Hash + ?Sized>(key: &K) -> Self {
        let mut hasher = StarlarkHasher::new();
        key.hash(&mut hasher);
        hasher.finish_small()
    }

    /// Directly create a new [`StarlarkHashValue`] using a hash.
    /// The expectation is that the key will be well-swizzled,
    /// or there may be many hash collisions.
    pub fn new_unchecked(hash: u32) -> Self {
        Self(hash)
    }

    /// Hash 64-bit integer.
    ///
    /// Input can also be a non-well swizzled hash to create better hash.
    pub(crate) const fn hash_64(h: u64) -> Self {
        // `fmix64` function from MurMur3 hash (which is in public domain).
        // https://github.com/aappleby/smhasher/blob/61a0530f28277f2e850bfc39600ce61d02b518de/src/MurmurHash3.cpp#L81

        let h = h ^ (h >> 33);
        let h = h.wrapping_mul(0xff51afd7ed558ccd);
        let h = h ^ (h >> 33);
        let h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
        let h = h ^ (h >> 33);

        StarlarkHashValue(h as u32)
    }

    pub fn get(self) -> u32 {
        self.0
    }

    /// Make u64 hash from this hash.
    ///
    /// The resulting hash should be good enough to be used in hashbrown hashtable.
    #[inline(always)]
    pub fn promote(self) -> u64 {
        mix_u32(self.0)
    }
}

impl<'a, Q: ?Sized> BorrowHashed<'a, Q> {
    /// Create a new [`BorrowHashed`] using the [`Hash`] trait
    /// for given key.
    pub fn new(key: &'a Q) -> Self
    where
        Q: Hash,
    {
        Self::new_unchecked(StarlarkHashValue::new(key), key)
    }

    /// Directly create a new [`BorrowHashed`] using a given hash value.
    /// If the hash does not correspond to the key, its will cause issues.
    pub fn new_unchecked(hash: StarlarkHashValue, key: &'a Q) -> Self {
        Self { hash, key }
    }

    /// Get the underlying hash.
    pub fn hash(&self) -> StarlarkHashValue {
        self.hash
    }

    /// Get the underlying key.
    pub fn key(&self) -> &'a Q {
        self.key
    }
}

impl<'a, Q: Clone> BorrowHashed<'a, Q> {
    /// Convert a borrowed hashed back to an unborrowed hashed using [`Clone`].
    pub fn unborrow_clone(&self) -> Hashed<Q> {
        Hashed::new_unchecked(self.hash, self.key.clone())
    }
}

impl<'a, Q: Copy> BorrowHashed<'a, Q> {
    /// Convert a borrowed hashed back to an unborrowed hashed using [`Copy`].
    pub fn unborrow_copy(&self) -> Hashed<Q> {
        Hashed::new_unchecked(self.hash, *self.key)
    }
}

impl<'a, Q, K> Equivalent<Hashed<K>> for BorrowHashed<'a, Q>
where
    Q: Equivalent<K> + ?Sized,
{
    fn equivalent(&self, key: &Hashed<K>) -> bool {
        self.hash == key.hash && self.key.equivalent(&key.key)
    }
}

impl<'a, Q: ?Sized> Hash for BorrowHashed<'a, Q> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state)
    }
}

impl<K> Deref for Hashed<K> {
    type Target = K;

    fn deref(&self) -> &Self::Target {
        &self.key
    }
}

// We deliberately know that this is a hash and value, so our Eq/Hash are fine
#[allow(clippy::derive_hash_xor_eq)]
impl<K> Hash for Hashed<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state)
    }
}

impl<K> Hashed<K> {
    /// Create a new [`Hashed`] value using the [`Hash`] of the key.
    pub fn new(key: K) -> Self
    where
        K: Hash,
    {
        Self::new_unchecked(StarlarkHashValue::new(&key), key)
    }

    /// Directly create a new [`Hashed`] using a given hash value.
    /// If the hash does not correspond to the key, its will cause issues.
    pub fn new_unchecked(hash: StarlarkHashValue, key: K) -> Self {
        Self { hash, key }
    }

    /// Get the underlying key.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Get the underlying key, as mutable.
    pub fn key_mut(&mut self) -> &mut K {
        &mut self.key
    }

    /// Get the underlying key taking ownership.
    pub fn into_key(self) -> K {
        self.key
    }

    /// Get the underlying hash.
    pub fn hash(&self) -> StarlarkHashValue {
        self.hash
    }

    /// Borrow this value, creating a [`BorrowHashed`].
    pub fn borrow(&self) -> BorrowHashed<K> {
        BorrowHashed::new_unchecked(self.hash, &self.key)
    }
}

#[cfg(test)]
mod tests {
    use indexmap::map::IndexMap;

    use crate::collections::{BorrowHashed, Hashed};

    #[test]
    fn borrow_and_hashed_equivalent() {
        let mut m = IndexMap::new();
        m.insert(Hashed::new(1), 'b');

        assert_eq!(m.get(&BorrowHashed::new(&1)), Some(&'b'));
    }
}
