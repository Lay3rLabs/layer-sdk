use core::fmt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;
use std::marker::PhantomData;

use cosmwasm_std::{Order, Record, StdResult};
use std::ops::Deref;

use layer_std::{GasMeter, GasResult};

use super::error::PlusResult;
use super::helpers::{namespaces_with_key, nested_namespaces_with_key};
use super::iter_helpers::{concat, deserialize_kv, deserialize_v, trim};
use super::{Bound, Key, KeyDeserialize, PrefixBound, Prefixer, PrimaryKey, RawBound};
use crate::ReadonlyStorage;

pub type PlusIterator<'c, T> = GasResult<Box<dyn Iterator<Item = PlusResult<T>> + 'c>>;
pub type GasIterator<'c, T> = GasResult<Box<dyn Iterator<Item = GasResult<T>> + 'c>>;

type DeserializeVFn<T> = fn(&dyn ReadonlyStorage, &[u8], Record) -> StdResult<Record<T>>;

type DeserializeKvFn<K, T> =
    fn(&dyn ReadonlyStorage, &[u8], Record) -> StdResult<(<K as KeyDeserialize>::Output, T)>;

pub fn default_deserializer_v<T: DeserializeOwned>(
    _: &dyn ReadonlyStorage,
    _: &[u8],
    raw: Record,
) -> StdResult<Record<T>> {
    deserialize_v(raw)
}

pub fn default_deserializer_kv<K: KeyDeserialize, T: DeserializeOwned>(
    _: &dyn ReadonlyStorage,
    _: &[u8],
    raw: Record,
) -> StdResult<(K::Output, T)> {
    deserialize_kv::<K, T>(raw)
}

#[derive(Clone)]
pub struct Prefix<K, T, B = Vec<u8>>
where
    K: KeyDeserialize,
    T: Serialize + DeserializeOwned,
{
    /// all namespaces prefixes and concatenated with the key
    storage_prefix: Vec<u8>,
    // see https://doc.rust-lang.org/std/marker/struct.PhantomData.html#unused-type-parameters for why this is needed
    data: PhantomData<(T, B)>,
    pk_name: Vec<u8>,
    de_fn_kv: DeserializeKvFn<K, T>,
    de_fn_v: DeserializeVFn<T>,
}

impl<K, T> Debug for Prefix<K, T>
where
    K: KeyDeserialize,
    T: Serialize + DeserializeOwned,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prefix")
            .field("storage_prefix", &self.storage_prefix)
            .field("pk_name", &self.pk_name)
            .finish_non_exhaustive()
    }
}

impl<K, T> Deref for Prefix<K, T>
where
    K: KeyDeserialize,
    T: Serialize + DeserializeOwned,
{
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.storage_prefix
    }
}

impl<K, T, B> Prefix<K, T, B>
where
    K: KeyDeserialize,
    T: Serialize + DeserializeOwned,
{
    pub fn new(top_name: &[u8], sub_names: &[Key]) -> Self {
        Prefix::with_deserialization_functions(
            top_name,
            sub_names,
            &[],
            default_deserializer_kv::<K, T>,
            default_deserializer_v,
        )
    }

    pub fn with_deserialization_functions(
        top_name: &[u8],
        sub_names: &[Key],
        pk_name: &[u8],
        de_fn_kv: DeserializeKvFn<K, T>,
        de_fn_v: DeserializeVFn<T>,
    ) -> Self {
        let storage_prefix = nested_namespaces_with_key(&[top_name], sub_names, b"");
        Prefix {
            storage_prefix,
            data: PhantomData,
            pk_name: pk_name.to_vec(),
            de_fn_kv,
            de_fn_v,
        }
    }
}

impl<'b, K, T, B> Prefix<K, T, B>
where
    B: PrimaryKey<'b>,
    K: KeyDeserialize,
    T: Serialize + DeserializeOwned,
{
    pub fn range_raw<'a>(
        &self,
        store: &'a dyn ReadonlyStorage,
        meter: &'a GasMeter,
        min: Option<Bound<'b, B>>,
        max: Option<Bound<'b, B>>,
        order: Order,
    ) -> PlusIterator<'a, Record<T>>
    where
        T: 'a,
    {
        let de_fn = self.de_fn_v;
        let pk_name = self.pk_name.clone();
        let mapped = range_with_prefix(
            store,
            meter,
            &self.storage_prefix,
            min.map(|b| b.to_raw_bound()),
            max.map(|b| b.to_raw_bound()),
            order,
        )?
        .map(move |r| {
            let kv = r?;
            Ok((de_fn)(store, &pk_name, kv)?)
        });
        Ok(Box::new(mapped))
    }

    pub fn keys_raw<'a>(
        &self,
        store: &'a dyn ReadonlyStorage,
        meter: &'a GasMeter,
        min: Option<Bound<'b, B>>,
        max: Option<Bound<'b, B>>,
        order: Order,
    ) -> GasIterator<'a, Vec<u8>> {
        let mapped = range_with_prefix(
            store,
            meter,
            &self.storage_prefix,
            min.map(|b| b.to_raw_bound()),
            max.map(|b| b.to_raw_bound()),
            order,
        )?
        .map(|r| Ok(r?.0));
        Ok(Box::new(mapped))
    }

    pub fn range<'a>(
        &self,
        store: &'a dyn ReadonlyStorage,
        meter: &'a GasMeter,
        min: Option<Bound<'b, B>>,
        max: Option<Bound<'b, B>>,
        order: Order,
    ) -> PlusIterator<'a, (K::Output, T)>
    where
        T: 'a,
        K::Output: 'static,
    {
        let de_fn = self.de_fn_kv;
        let pk_name = self.pk_name.clone();
        let mapped = range_with_prefix(
            store,
            meter,
            &self.storage_prefix,
            min.map(|b| b.to_raw_bound()),
            max.map(|b| b.to_raw_bound()),
            order,
        )?
        .map(move |r| {
            let kv = r?;
            Ok((de_fn)(store, &pk_name, kv)?)
        });
        Ok(Box::new(mapped))
    }

    pub fn keys<'a>(
        &self,
        store: &'a dyn ReadonlyStorage,
        meter: &'a GasMeter,
        min: Option<Bound<'b, B>>,
        max: Option<Bound<'b, B>>,
        order: Order,
    ) -> PlusIterator<'a, K::Output>
    where
        T: 'a,
        K::Output: 'static,
    {
        let de_fn = self.de_fn_kv;
        let pk_name = self.pk_name.clone();
        let mapped = range_with_prefix(
            store,
            meter,
            &self.storage_prefix,
            min.map(|b| b.to_raw_bound()),
            max.map(|b| b.to_raw_bound()),
            order,
        )?
        .map(move |r| {
            let kv = r?;
            let (k, _) = (de_fn)(store, &pk_name, kv)?;
            Ok(k)
        });
        Ok(Box::new(mapped))
    }
}

pub fn range_with_prefix<'a>(
    storage: &'a dyn ReadonlyStorage,
    meter: &'a GasMeter,
    namespace: &[u8],
    start: Option<RawBound>,
    end: Option<RawBound>,
    order: Order,
) -> GasIterator<'a, Record> {
    let start = calc_start_bound(namespace, start);
    let end = calc_end_bound(namespace, end);

    // get iterator from storage
    let base_iterator = storage.range(meter, Some(&start), Some(&end), order)?;

    // make a copy for the closure to handle lifetimes safely
    let prefix = namespace.to_vec();
    let mapped = base_iterator.map(move |r| {
        let (k, v) = r?;
        Ok((trim(&prefix, &k), v))
    });
    Ok(Box::new(mapped))
}

fn calc_start_bound(namespace: &[u8], bound: Option<RawBound>) -> Vec<u8> {
    match bound {
        None => namespace.to_vec(),
        // this is the natural limits of the underlying Storage
        Some(RawBound::Inclusive(limit)) => concat(namespace, &limit),
        Some(RawBound::Exclusive(limit)) => concat(namespace, &extend_one_byte(&limit)),
    }
}

fn calc_end_bound(namespace: &[u8], bound: Option<RawBound>) -> Vec<u8> {
    match bound {
        None => increment_last_byte(namespace),
        // this is the natural limits of the underlying Storage
        Some(RawBound::Exclusive(limit)) => concat(namespace, &limit),
        Some(RawBound::Inclusive(limit)) => concat(namespace, &extend_one_byte(&limit)),
    }
}

pub fn namespaced_prefix_range<'a, 'c, K: Prefixer<'a>>(
    storage: &'c dyn ReadonlyStorage,
    meter: &'c GasMeter,
    namespace: &[u8],
    start: Option<PrefixBound<'a, K>>,
    end: Option<PrefixBound<'a, K>>,
    order: Order,
) -> GasIterator<'c, Record> {
    let prefix = namespaces_with_key(&[namespace], &[]);
    let start = calc_prefix_start_bound(&prefix, start);
    let end = calc_prefix_end_bound(&prefix, end);

    // get iterator from storage
    let base_iterator = storage.range(meter, Some(&start), Some(&end), order)?;

    // make a copy for the closure to handle lifetimes safely
    let mapped = base_iterator.map(move |r| {
        let (k, v) = r?;
        Ok((trim(&prefix, &k), v))
    });
    Ok(Box::new(mapped))
}

fn calc_prefix_start_bound<'a, K: Prefixer<'a>>(
    namespace: &[u8],
    bound: Option<PrefixBound<'a, K>>,
) -> Vec<u8> {
    match bound.map(|b| b.to_raw_bound()) {
        None => namespace.to_vec(),
        // this is the natural limits of the underlying Storage
        Some(RawBound::Inclusive(limit)) => concat(namespace, &limit),
        Some(RawBound::Exclusive(limit)) => concat(namespace, &increment_last_byte(&limit)),
    }
}

fn calc_prefix_end_bound<'a, K: Prefixer<'a>>(
    namespace: &[u8],
    bound: Option<PrefixBound<'a, K>>,
) -> Vec<u8> {
    match bound.map(|b| b.to_raw_bound()) {
        None => increment_last_byte(namespace),
        // this is the natural limits of the underlying Storage
        Some(RawBound::Exclusive(limit)) => concat(namespace, &limit),
        Some(RawBound::Inclusive(limit)) => concat(namespace, &increment_last_byte(&limit)),
    }
}

fn extend_one_byte(limit: &[u8]) -> Vec<u8> {
    let mut v = limit.to_vec();
    v.push(0);
    v
}

/// Returns a new vec of same length and last byte incremented by one
/// If last bytes are 255, we handle overflow up the chain.
/// If all bytes are 255, this returns wrong data - but that is never possible as a namespace
fn increment_last_byte(input: &[u8]) -> Vec<u8> {
    let mut copy = input.to_vec();
    // zero out all trailing 255, increment first that is not such
    for i in (0..input.len()).rev() {
        if copy[i] == 255 {
            copy[i] = 0;
        } else {
            copy[i] += 1;
            break;
        }
    }
    copy
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{MemoryStore, PersistentStorage, Storage};

    #[test]
    fn ensure_proper_range_bounds() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // manually create this - not testing nested prefixes here
        let prefix: Prefix<Vec<u8>, u64> = Prefix {
            storage_prefix: b"foo".to_vec(),
            data: PhantomData::<(u64, _)>,
            pk_name: vec![],
            de_fn_kv: |_, _, kv| deserialize_kv::<Vec<u8>, u64>(kv),
            de_fn_v: |_, _, kv| deserialize_v(kv),
        };

        // set some data, we care about "foo" prefix
        store.set(meter, b"foobar", b"1").unwrap();
        store.set(meter, b"foora", b"2").unwrap();
        store.set(meter, b"foozi", b"3").unwrap();
        // these shouldn't match
        store.set(meter, b"foply", b"100").unwrap();
        store.set(meter, b"font", b"200").unwrap();

        let expected = vec![
            (b"bar".to_vec(), 1u64),
            (b"ra".to_vec(), 2u64),
            (b"zi".to_vec(), 3u64),
        ];
        let expected_reversed: Vec<(Vec<u8>, u64)> = expected.iter().rev().cloned().collect();

        // let's do the basic sanity check
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        assert_eq!(&expected, &res.unwrap());
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(&store, meter, None, None, Order::Descending)
            .unwrap()
            .collect();
        assert_eq!(&expected_reversed, &res.unwrap());

        // now let's check some ascending ranges
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                Some(Bound::inclusive(b"ra".to_vec())),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected[1..], res.unwrap().as_slice());
        // skip excluded
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                Some(Bound::exclusive(b"ra".to_vec())),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected[2..], res.unwrap().as_slice());
        // if we exclude something a little lower, we get matched
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                Some(Bound::exclusive(b"r".to_vec())),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected[1..], res.unwrap().as_slice());

        // now let's check some descending ranges
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                None,
                Some(Bound::inclusive(b"ra".to_vec())),
                Order::Descending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected_reversed[1..], res.unwrap().as_slice());
        // skip excluded
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                None,
                Some(Bound::exclusive(b"ra".to_vec())),
                Order::Descending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected_reversed[2..], res.unwrap().as_slice());
        // if we exclude something a little higher, we get matched
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                None,
                Some(Bound::exclusive(b"rb".to_vec())),
                Order::Descending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected_reversed[1..], res.unwrap().as_slice());

        // now test when both sides are set
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                Some(Bound::inclusive(b"ra".to_vec())),
                Some(Bound::exclusive(b"zi".to_vec())),
                Order::Ascending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected[1..2], res.unwrap().as_slice());
        // and descending
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                Some(Bound::inclusive(b"ra".to_vec())),
                Some(Bound::exclusive(b"zi".to_vec())),
                Order::Descending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected[1..2], res.unwrap().as_slice());
        // Include both sides
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                Some(Bound::inclusive(b"ra".to_vec())),
                Some(Bound::inclusive(b"zi".to_vec())),
                Order::Descending,
            )
            .unwrap()
            .collect();
        assert_eq!(&expected_reversed[..2], res.unwrap().as_slice());
        // Exclude both sides
        let res: PlusResult<Vec<_>> = prefix
            .range_raw(
                &store,
                meter,
                Some(Bound::exclusive(b"ra".to_vec())),
                Some(Bound::exclusive(b"zi".to_vec())),
                Order::Ascending,
            )
            .unwrap()
            .collect();
        assert_eq!(res.unwrap().as_slice(), &[]);
    }
}

#[test]
fn prefix_debug() {
    let prefix: Prefix<String, String> = Prefix::new(b"lol", &[Key::Val8([8; 1])]);
    assert_eq!(
        format!("{:?}", prefix),
        "Prefix { storage_prefix: [0, 3, 108, 111, 108, 0, 1, 8], pk_name: [], .. }"
    );
}
