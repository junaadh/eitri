//! Explicit context-owned memory accounting and fallible vector growth.

/// A memory request exceeded the active quota or the allocator refused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfMemory;

/// Accounted live bytes and their peak. The caller supplies the already-live
/// context and active-artifact bytes at the start of an operation.
#[derive(Debug)]
pub struct MemoryBudget {
    limit: Option<usize>,
    used: usize,
    peak: usize,
}

impl MemoryBudget {
    /// A zero limit is unlimited. Reject a context already over its limit.
    pub fn new(limit: usize, used: usize) -> Result<Self, OutOfMemory> {
        let limit = (limit != 0).then_some(limit);
        if limit.is_some_and(|limit| used > limit) {
            return Err(OutOfMemory);
        }
        Ok(Self {
            limit,
            used,
            peak: used,
        })
    }

    pub fn used(&self) -> usize {
        self.used
    }
    pub fn peak(&self) -> usize {
        self.peak
    }

    /// Charge before an allocation or external ownership transfer.
    pub fn charge(&mut self, bytes: usize) -> Result<(), OutOfMemory> {
        let used = self.used.checked_add(bytes).ok_or(OutOfMemory)?;
        if self.limit.is_some_and(|limit| used > limit) {
            return Err(OutOfMemory);
        }
        self.used = used;
        self.peak = self.peak.max(used);
        Ok(())
    }

    /// Release a previously charged allocation.
    pub fn release(&mut self, bytes: usize) {
        self.used = self
            .used
            .checked_sub(bytes)
            .expect("released bytes were charged");
    }

    /// Reserve string storage, charging both allocations at reallocation peak.
    /// The string's existing capacity must already be charged.
    pub fn reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
    ) -> Result<(), OutOfMemory> {
        let target = value.len().checked_add(additional).ok_or(OutOfMemory)?;
        if target <= value.capacity() {
            return Ok(());
        }
        let old = value.capacity();
        self.charge(target)?;
        if value.try_reserve_exact(additional).is_err() {
            self.release(target);
            return Err(OutOfMemory);
        }
        let actual = value.capacity();
        if actual > target && self.charge(actual - target).is_err() {
            self.used = self.used.checked_add(actual - target).ok_or(OutOfMemory)?;
            self.peak = self.peak.max(self.used);
            self.release(old);
            return Err(OutOfMemory);
        }
        self.release(old);
        Ok(())
    }

    /// Conservatively account a standard hash table's buckets and control
    /// bytes from its usable capacity. The estimate includes alignment and
    /// the terminating control group; allocator bookkeeping is excluded.
    pub fn hash_table_bytes<T>(capacity: usize) -> Result<usize, OutOfMemory> {
        if capacity == 0 {
            return Ok(0);
        }
        let buckets = capacity
            .checked_mul(8)
            .and_then(|n| n.checked_add(6))
            .map(|n| n / 7)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(OutOfMemory)?
            .max(4);
        buckets
            .checked_mul(size_of::<T>().checked_add(1).ok_or(OutOfMemory)?)
            .and_then(|n| n.checked_add(16))
            .and_then(|n| n.checked_add(std::mem::align_of::<T>().max(16)))
            .ok_or(OutOfMemory)
    }

    /// Reserve a hash table before insertion. Existing table storage must
    /// already be charged. The conservative estimate covers bucket growth
    /// and overlap with the old table during reallocation.
    pub fn reserve_map<K, V, S>(
        &mut self,
        map: &mut std::collections::HashMap<K, V, S>,
        additional: usize,
    ) -> Result<(), OutOfMemory>
    where
        K: Eq + std::hash::Hash,
        S: std::hash::BuildHasher,
    {
        let requested = map.len().checked_add(additional).ok_or(OutOfMemory)?;
        if requested <= map.capacity() {
            return Ok(());
        }
        let old = Self::hash_table_bytes::<(K, V)>(map.capacity())?;
        let bound = requested.max(map.capacity().checked_mul(2).ok_or(OutOfMemory)?);
        let reserved = Self::hash_table_bytes::<(K, V)>(bound)?;
        self.charge(reserved)?;
        if map.try_reserve(additional).is_err() {
            self.release(reserved);
            return Err(OutOfMemory);
        }
        let actual = Self::hash_table_bytes::<(K, V)>(map.capacity())?;
        if actual > reserved && self.charge(actual - reserved).is_err() {
            self.used = self
                .used
                .checked_add(actual - reserved)
                .ok_or(OutOfMemory)?;
            self.peak = self.peak.max(self.used);
            self.release(old);
            return Err(OutOfMemory);
        }
        if reserved > actual {
            self.release(reserved - actual);
        }
        self.release(old);
        Ok(())
    }

    /// Reserve a vector without allowing its old and new allocations to
    /// overlap outside the budget. Existing vector capacity must already be
    /// charged. On failure, the vector and the live-byte count are unchanged.
    pub fn reserve_vec<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), OutOfMemory> {
        let target = values.len().checked_add(additional).ok_or(OutOfMemory)?;
        if target <= values.capacity() || size_of::<T>() == 0 {
            return Ok(());
        }
        let old = values
            .capacity()
            .checked_mul(size_of::<T>())
            .ok_or(OutOfMemory)?;
        let requested = target.checked_mul(size_of::<T>()).ok_or(OutOfMemory)?;
        self.charge(requested)?;
        if values.try_reserve_exact(additional).is_err() {
            self.release(requested);
            return Err(OutOfMemory);
        }
        let actual = values
            .capacity()
            .checked_mul(size_of::<T>())
            .ok_or(OutOfMemory)?;
        // `try_reserve_exact` with the global allocator normally gives exactly
        // the requested capacity. Account any allocator-supplied excess too.
        if actual > requested && self.charge(actual - requested).is_err() {
            // Preserve contents. The allocator may keep excess capacity; it is
            // still accounted, and the caller must discard this operation.
            self.used = self
                .used
                .checked_add(actual - requested)
                .ok_or(OutOfMemory)?;
            self.peak = self.peak.max(self.used);
            self.release(old);
            return Err(OutOfMemory);
        }
        self.release(old);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_growth_preserves_values_and_accounting() {
        let mut values = vec![1u64, 2];
        let old = values.capacity() * size_of::<u64>();
        let mut budget = MemoryBudget::new(old + 8, old).unwrap();
        assert_eq!(budget.reserve_vec(&mut values, 2), Err(OutOfMemory));
        assert_eq!(values, [1, 2]);
        assert_eq!(budget.used(), old);
    }

    #[test]
    fn growth_charges_reallocation_peak_and_retained_capacity() {
        let mut values = vec![1u64, 2];
        let old = values.capacity() * size_of::<u64>();
        let mut budget = MemoryBudget::new(0, old).unwrap();
        budget.reserve_vec(&mut values, 2).unwrap();
        assert_eq!(budget.used(), values.capacity() * size_of::<u64>());
        assert!(budget.peak() >= old + 4 * size_of::<u64>());
    }
}
