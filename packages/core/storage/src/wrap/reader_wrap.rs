use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::iter;
use std::iter::Peekable;
use std::ops::{Bound, RangeBounds};
use tracing::trace_span;

use cosmwasm_std::{Order, Record};
use layer_std::{GasMeter, GasResult};

use super::{BTreeMapPairRef, Delta, Op};
use crate::ReadonlyStorage;

pub(crate) struct ReaderWrapper {
    /// read-only access to backing storage
    // storage: Box<dyn ReadonlyStorage + 'a>,
    /// these are local changes not flushed to backing storage
    pub(crate) local_state: BTreeMap<Vec<u8>, Delta>,
}

impl ReaderWrapper {
    pub(crate) fn new() -> Self {
        ReaderWrapper {
            local_state: BTreeMap::new(),
        }
    }

    /// prepares this transaction to be committed to storage
    /// Consumes local cache and converts it to something than can be applied to another storage
    pub(crate) fn prepare(self) -> Vec<Op> {
        self.local_state.into_iter().map(Op::from_delta).collect()
    }

    pub(crate) fn get(
        &self,
        storage: &dyn ReadonlyStorage,
        meter: &GasMeter,
        key: &[u8],
    ) -> GasResult<Option<Vec<u8>>> {
        match self.local_state.get(key) {
            Some(val) => match val {
                Delta::Set { value } => Ok(Some(value.clone())),
                Delta::Delete {} => Ok(None),
            },
            None => storage.get(meter, key),
        }
    }

    /// range allows iteration over a set of keys, either forwards or backwards
    pub(crate) fn range<'b>(
        &'b self,
        storage: &'b dyn ReadonlyStorage,
        meter: &'b GasMeter,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> GasResult<Box<dyn Iterator<Item = GasResult<Record>> + 'b>> {
        let bounds = range_bounds(start, end);

        // BTreeMap.range panics if range is start > end.
        // However, this cases represent just empty range and we treat it as such.
        let local: Box<dyn Iterator<Item = BTreeMapPairRef<Delta>>> =
            match (bounds.start_bound(), bounds.end_bound()) {
                (Bound::Included(start), Bound::Excluded(end)) if start > end => {
                    Box::new(iter::empty())
                }
                _ => {
                    let local_raw = self.local_state.range(bounds);
                    match order {
                        Order::Ascending => Box::new(local_raw),
                        Order::Descending => Box::new(local_raw.rev()),
                    }
                }
            };

        let base = storage.range(meter, start, end, order)?;
        let merged = MergeOverlay::new(local, base, order);
        Ok(Box::new(merged))
    }

    pub(crate) fn set(&mut self, _meter: &GasMeter, key: &[u8], value: &[u8]) -> GasResult<()> {
        let delta = Delta::Set {
            value: value.to_vec(),
        };
        self.local_state.insert(key.to_vec(), delta);
        Ok(())
    }

    pub(crate) fn remove(&mut self, _meter: &GasMeter, key: &[u8]) -> GasResult<()> {
        let delta = Delta::Delete {};
        self.local_state.insert(key.to_vec(), delta);
        Ok(())
    }
}

struct MergeOverlay<'a, L, R>
where
    L: Iterator<Item = BTreeMapPairRef<'a, Delta>>,
    R: Iterator<Item = GasResult<Record>>,
{
    left: Peekable<L>,
    right: Peekable<R>,
    order: Order,
}

impl<'a, L, R> MergeOverlay<'a, L, R>
where
    L: Iterator<Item = BTreeMapPairRef<'a, Delta>>,
    R: Iterator<Item = GasResult<Record>>,
{
    fn new(left: L, right: R, order: Order) -> Self {
        MergeOverlay {
            left: left.peekable(),
            right: right.peekable(),
            order,
        }
    }

    fn pick_match(&mut self, lkey: Vec<u8>, rkey: Vec<u8>) -> Option<GasResult<Record>> {
        // compare keys - result is such that Ordering::Less => return left side
        let order = match self.order {
            Order::Ascending => lkey.cmp(&rkey),
            Order::Descending => rkey.cmp(&lkey),
        };

        // left must be translated and filtered before return, not so with right
        match order {
            Ordering::Less => self.take_left(),
            Ordering::Equal => {
                //
                let _ = self.right.next();
                self.take_left()
            }
            Ordering::Greater => self.right.next(),
        }
    }

    /// take_left must only be called when we know self.left.next() will return Some
    fn take_left(&mut self) -> Option<GasResult<Record>> {
        let (lkey, lval) = self.left.next().unwrap();
        match lval {
            Delta::Set { value } => Some(Ok((lkey.clone(), value.clone()))),
            Delta::Delete {} => self.next(),
        }
    }
}

impl<'a, L, R> Iterator for MergeOverlay<'a, L, R>
where
    L: Iterator<Item = BTreeMapPairRef<'a, Delta>>,
    R: Iterator<Item = GasResult<Record>>,
{
    type Item = GasResult<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        let _span = trace_span!("next").entered();
        let (left, right) = (self.left.peek(), self.right.peek());
        match (left, right) {
            (Some(litem), Some(ritem)) => {
                let (lkey, _) = litem;
                let rkey = match ritem {
                    Ok((k, _)) => k,
                    Err(e) => return Some(Err(e.clone())),
                };

                // we just use cloned keys to avoid double mutable references
                // (we must release the return value from peek, before beginning to call next or other mut methods
                let (l, r) = (lkey.to_vec(), rkey.to_vec());
                self.pick_match(l, r)
            }
            (Some(_), None) => self.take_left(),
            (None, Some(_)) => self.right.next(),
            (None, None) => None,
        }
    }
}

fn range_bounds(start: Option<&[u8]>, end: Option<&[u8]>) -> impl RangeBounds<Vec<u8>> {
    (
        start.map_or(Bound::Unbounded, |x| Bound::Included(x.to_vec())),
        end.map_or(Bound::Unbounded, |x| Bound::Excluded(x.to_vec())),
    )
}
