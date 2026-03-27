use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;

use cosmwasm_std::{from_json, Addr, CustomQuery, QuerierWrapper, Record, StdResult};

use layer_std::{GasMeter, GasResult};

use super::error::{PlusError, PlusResult};
use super::helpers::query_raw;
use super::iter_helpers::{deserialize_kv, deserialize_v};
use super::path::Path;
use super::prefix::{namespaced_prefix_range, GasIterator, PlusIterator, Prefix};
use super::{Bound, Key, KeyDeserialize, PrefixBound, Prefixer, PrimaryKey};
use crate::{ReadonlyStorage, Storage};

#[derive(Debug, Clone)]
pub struct Map<'a, K, T> {
    namespace: &'a [u8],
    // see https://doc.rust-lang.org/std/marker/struct.PhantomData.html#unused-type-parameters for why this is needed
    key_type: PhantomData<K>,
    data_type: PhantomData<T>,
}

impl<'a, K, T> Map<'a, K, T> {
    pub const fn new(namespace: &'a str) -> Self {
        Map {
            namespace: namespace.as_bytes(),
            data_type: PhantomData,
            key_type: PhantomData,
        }
    }

    pub fn namespace(&self) -> &'a [u8] {
        self.namespace
    }
}

impl<'a, K, T> Map<'a, K, T>
where
    T: Serialize + DeserializeOwned,
    K: PrimaryKey<'a>,
{
    pub fn key(&self, k: K) -> Path<T> {
        Path::new(
            self.namespace,
            &k.key().iter().map(Key::as_ref).collect::<Vec<_>>(),
        )
    }

    pub(crate) fn no_prefix_raw(&self) -> Prefix<Vec<u8>, T, K> {
        Prefix::new(self.namespace, &[])
    }

    pub fn save(
        &self,
        store: &mut dyn Storage,
        meter: &GasMeter,
        k: K,
        data: &T,
    ) -> PlusResult<()> {
        self.key(k).save(store, meter, data)
    }

    pub fn remove(&self, store: &mut dyn Storage, meter: &GasMeter, k: K) -> GasResult<()> {
        self.key(k).remove(store, meter)
    }

    /// load will return an error if no data is set at the given key, or on parse error
    pub fn load(&self, store: &dyn ReadonlyStorage, meter: &GasMeter, k: K) -> PlusResult<T> {
        self.key(k).load(store, meter)
    }

    /// may_load will parse the data stored at the key if present, returns Ok(None) if no data there.
    /// returns an error on issues parsing
    pub fn may_load(
        &self,
        store: &dyn ReadonlyStorage,
        meter: &GasMeter,
        k: K,
    ) -> PlusResult<Option<T>> {
        self.key(k).may_load(store, meter)
    }

    /// has returns true or false if any data is at this key, without parsing or interpreting the
    /// contents.
    pub fn has(&self, store: &dyn ReadonlyStorage, meter: &GasMeter, k: K) -> GasResult<bool> {
        self.key(k).has(store, meter)
    }

    /// Loads the data, perform the specified action, and store the result
    /// in the database. This is shorthand for some common sequences, which may be useful.
    ///
    /// If the data exists, `action(Some(value))` is called. Otherwise `action(None)` is called.
    pub fn update<A, E>(
        &self,
        store: &mut dyn Storage,
        meter: &GasMeter,
        k: K,
        action: A,
    ) -> Result<T, E>
    where
        A: FnOnce(Option<T>) -> Result<T, E>,
        E: From<PlusError>,
    {
        self.key(k).update(store, meter, action)
    }

    /// If you import the proper Map from the remote contract, this will let you read the data
    /// from a remote contract in a type-safe way using WasmQuery::RawQuery
    pub fn query<Q: CustomQuery>(
        &self,
        querier: &QuerierWrapper<Q>,
        remote_contract: Addr,
        k: K,
    ) -> StdResult<Option<T>> {
        let key = self.key(k).storage_key.into();
        let result = query_raw(querier, remote_contract, key)?;
        if result.is_empty() {
            Ok(None)
        } else {
            from_json(&result).map(Some)
        }
    }

    /// Clears the map, removing all elements.
    pub fn clear(&self, store: &mut dyn Storage, meter: &GasMeter) -> GasResult<()> {
        const TAKE: usize = 10;
        let mut cleared = false;

        while !cleared {
            let paths = self
                .no_prefix_raw()
                .keys_raw(
                    store.as_ref(),
                    meter,
                    None,
                    None,
                    cosmwasm_std::Order::Ascending,
                )?
                .map(|r| {
                    let raw_key = r?;
                    Ok(Path::<T>::new(self.namespace, &[raw_key.as_slice()]))
                })
                // Take just TAKE elements to prevent possible heap overflow if the Map is big.
                .take(TAKE)
                .collect::<Vec<_>>();
            let len = paths.len();

            // call remove on them all and error if any error
            paths
                .into_iter()
                .map(|path| store.remove(meter, &path?))
                .collect::<GasResult<Vec<_>>>()?;

            cleared = len < TAKE;
        }
        Ok(())
    }

    /// Returns `true` if the map is empty.
    pub fn is_empty(&self, store: &dyn ReadonlyStorage, meter: &GasMeter) -> GasResult<bool> {
        let empty = self
            .no_prefix_raw()
            .keys_raw(store, meter, None, None, cosmwasm_std::Order::Ascending)?
            .next()
            .is_none();
        Ok(empty)
    }
}

impl<'a, K, T> Map<'a, K, T>
where
    T: Serialize + DeserializeOwned,
    K: PrimaryKey<'a>,
{
    pub fn sub_prefix(&self, p: K::SubPrefix) -> Prefix<K::SuperSuffix, T, K::SuperSuffix> {
        Prefix::new(self.namespace, &p.prefix())
    }

    pub fn prefix(&self, p: K::Prefix) -> Prefix<K::Suffix, T, K::Suffix> {
        Prefix::new(self.namespace, &p.prefix())
    }
}

// short-cut for simple keys, rather than .prefix(()).range_raw(...)
impl<'a, K, T> Map<'a, K, T>
where
    T: Serialize + DeserializeOwned,
    // TODO: this should only be when K::Prefix == ()
    // Other cases need to call prefix() first
    K: PrimaryKey<'a>,
{
    /// While `range_raw` over a `prefix` fixes the prefix to one element and iterates over the
    /// remaining, `prefix_range_raw` accepts bounds for the lowest and highest elements of the `Prefix`
    /// itself, and iterates over those (inclusively or exclusively, depending on `PrefixBound`).
    /// There are some issues that distinguish these two, and blindly casting to `Vec<u8>` doesn't
    /// solve them.
    pub fn prefix_range_raw<'c>(
        &self,
        store: &'c dyn ReadonlyStorage,
        meter: &'c GasMeter,
        min: Option<PrefixBound<'a, K::Prefix>>,
        max: Option<PrefixBound<'a, K::Prefix>>,
        order: cosmwasm_std::Order,
    ) -> PlusIterator<'c, Record<T>>
    where
        T: 'c,
        'a: 'c,
    {
        let mapped =
            namespaced_prefix_range(store, meter, self.namespace, min, max, order)?.map(|r| {
                let args = r?;
                Ok(deserialize_v(args)?)
            });
        Ok(Box::new(mapped))
    }
}

impl<'a, K, T> Map<'a, K, T>
where
    T: Serialize + DeserializeOwned,
    K: PrimaryKey<'a> + KeyDeserialize,
{
    /// While `range` over a `prefix` fixes the prefix to one element and iterates over the
    /// remaining, `prefix_range` accepts bounds for the lowest and highest elements of the
    /// `Prefix` itself, and iterates over those (inclusively or exclusively, depending on
    /// `PrefixBound`).
    /// There are some issues that distinguish these two, and blindly casting to `Vec<u8>` doesn't
    /// solve them.
    pub fn prefix_range<'c>(
        &self,
        store: &'c dyn ReadonlyStorage,
        meter: &'c GasMeter,
        min: Option<PrefixBound<'a, K::Prefix>>,
        max: Option<PrefixBound<'a, K::Prefix>>,
        order: cosmwasm_std::Order,
    ) -> PlusIterator<'c, (K::Output, T)>
    where
        T: 'c,
        'a: 'c,
        K: 'c,
        K::Output: 'static,
    {
        let mapped =
            namespaced_prefix_range(store, meter, self.namespace, min, max, order)?.map(|r| {
                let args = r?;
                Ok(deserialize_kv::<K, T>(args)?)
            });
        Ok(Box::new(mapped))
    }

    fn no_prefix(&self) -> Prefix<K, T, K> {
        Prefix::new(self.namespace, &[])
    }
}

impl<'a, K, T> Map<'a, K, T>
where
    T: Serialize + DeserializeOwned,
    K: PrimaryKey<'a>,
{
    pub fn range_raw<'c>(
        &self,
        store: &'c dyn ReadonlyStorage,
        meter: &'c GasMeter,
        min: Option<Bound<'a, K>>,
        max: Option<Bound<'a, K>>,
        order: cosmwasm_std::Order,
    ) -> PlusIterator<'c, Record<T>>
    where
        T: 'c,
    {
        self.no_prefix_raw()
            .range_raw(store, meter, min, max, order)
    }

    pub fn keys_raw<'c>(
        &self,
        store: &'c dyn ReadonlyStorage,
        meter: &'c GasMeter,
        min: Option<Bound<'a, K>>,
        max: Option<Bound<'a, K>>,
        order: cosmwasm_std::Order,
    ) -> GasIterator<'c, Vec<u8>>
    where
        T: 'c,
    {
        self.no_prefix_raw().keys_raw(store, meter, min, max, order)
    }
}

impl<'a, K, T> Map<'a, K, T>
where
    T: Serialize + DeserializeOwned,
    K: PrimaryKey<'a> + KeyDeserialize,
{
    pub fn range<'c>(
        &self,
        store: &'c dyn ReadonlyStorage,
        meter: &'c GasMeter,
        min: Option<Bound<'a, K>>,
        max: Option<Bound<'a, K>>,
        order: cosmwasm_std::Order,
    ) -> PlusIterator<'c, (K::Output, T)>
    where
        T: 'c,
        K::Output: 'static,
    {
        self.no_prefix().range(store, meter, min, max, order)
    }

    pub fn keys<'c>(
        &self,
        store: &'c dyn ReadonlyStorage,
        meter: &'c GasMeter,
        min: Option<Bound<'a, K>>,
        max: Option<Bound<'a, K>>,
        order: cosmwasm_std::Order,
    ) -> PlusIterator<'c, K::Output>
    where
        T: 'c,
        K::Output: 'static,
    {
        self.no_prefix().keys(store, meter, min, max, order)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::ops::Deref;

    use cosmwasm_std::{to_json_binary, Order, StdError};

    use crate::plus::{Bounder, IntKey};
    use crate::{MemoryStore, PersistentStorage, Storage};

    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct Data {
        pub name: String,
        pub age: i32,
    }

    const PEOPLE: Map<&[u8], Data> = Map::new("people");
    const PEOPLE_STR_KEY: &str = "people2";
    const PEOPLE_STR: Map<&str, Data> = Map::new(PEOPLE_STR_KEY);
    const PEOPLE_ID: Map<u32, Data> = Map::new("people_id");
    const SIGNED_ID: Map<i32, Data> = Map::new("signed_id");

    const ALLOWANCE: Map<(&[u8], &[u8]), u64> = Map::new("allow");

    const TRIPLE: Map<(&[u8], u8, &str), u64> = Map::new("triple");

    #[test]
    fn create_path() {
        let path = PEOPLE.key(b"john");
        let key = path.deref();
        // this should be prefixed(people) || john
        assert_eq!("people".len() + "john".len() + 2, key.len());
        assert_eq!(b"people".to_vec().as_slice(), &key[2..8]);
        assert_eq!(b"john".to_vec().as_slice(), &key[8..]);

        let path = ALLOWANCE.key((b"john", b"maria"));
        let key = path.deref();
        // this should be prefixed(allow) || prefixed(john) || maria
        assert_eq!(
            "allow".len() + "john".len() + "maria".len() + 2 * 2,
            key.len()
        );
        assert_eq!(b"allow".to_vec().as_slice(), &key[2..7]);
        assert_eq!(b"john".to_vec().as_slice(), &key[9..13]);
        assert_eq!(b"maria".to_vec().as_slice(), &key[13..]);

        let path = TRIPLE.key((b"john", 8u8, "pedro"));
        let key = path.deref();
        // this should be prefixed(allow) || prefixed(john) || maria
        assert_eq!(
            "triple".len() + "john".len() + 1 + "pedro".len() + 2 * 3,
            key.len()
        );
        assert_eq!(b"triple".to_vec().as_slice(), &key[2..8]);
        assert_eq!(b"john".to_vec().as_slice(), &key[10..14]);
        assert_eq!(8u8.to_cw_bytes(), &key[16..17]);
        assert_eq!(b"pedro".to_vec().as_slice(), &key[17..]);
    }

    #[test]
    fn save_and_load() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on one key
        let john = PEOPLE.key(b"john");
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        assert_eq!(None, john.may_load(&store, meter).unwrap());
        john.save(&mut store, meter, &data).unwrap();
        assert_eq!(data, john.load(&store, meter).unwrap());

        // nothing on another key
        assert_eq!(None, PEOPLE.may_load(&store, meter, b"jack").unwrap());

        // same named path gets the data
        assert_eq!(data, PEOPLE.load(&store, meter, b"john").unwrap());

        // removing leaves us empty
        john.remove(&mut store, meter).unwrap();
        assert_eq!(None, john.may_load(&store, meter).unwrap());
    }

    #[test]
    fn existence() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // set data in proper format
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, meter, b"john", &data).unwrap();

        // set and remove it
        PEOPLE.save(&mut store, meter, b"removed", &data).unwrap();
        PEOPLE.remove(&mut store, meter, b"removed").unwrap();

        // invalid, but non-empty data
        store
            .set(meter, &PEOPLE.key(b"random"), b"random-data")
            .unwrap();

        // any data, including invalid or empty is returned as "has"
        assert!(PEOPLE.has(&store, meter, b"john").unwrap());
        assert!(PEOPLE.has(&store, meter, b"random").unwrap());

        // if nothing was written, it is false
        assert!(!PEOPLE.has(&store, meter, b"never-writen").unwrap());
        assert!(!PEOPLE.has(&store, meter, b"removed").unwrap());
    }

    #[test]
    fn composite_keys() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on a composite key
        let allow = ALLOWANCE.key((b"owner", b"spender"));
        assert_eq!(None, allow.may_load(&store, meter).unwrap());
        allow.save(&mut store, meter, &1234).unwrap();
        assert_eq!(1234, allow.load(&store, meter).unwrap());

        // not under other key
        let different = ALLOWANCE
            .may_load(&store, meter, (b"owners", b"pender"))
            .unwrap();
        assert_eq!(None, different);

        // matches under a proper copy
        let same = ALLOWANCE
            .load(&store, meter, (b"owner", b"spender"))
            .unwrap();
        assert_eq!(1234, same);
    }

    #[test]
    fn triple_keys() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on a triple composite key
        let triple = TRIPLE.key((b"owner", 10u8, "recipient"));
        assert_eq!(None, triple.may_load(&store, meter).unwrap());
        triple.save(&mut store, meter, &1234).unwrap();
        assert_eq!(1234, triple.load(&store, meter).unwrap());

        // not under other key
        let different = TRIPLE
            .may_load(&store, meter, (b"owners", 10u8, "ecipient"))
            .unwrap();
        assert_eq!(None, different);

        // matches under a proper copy
        let same = TRIPLE
            .load(&store, meter, (b"owner", 10u8, "recipient"))
            .unwrap();
        assert_eq!(1234, same);
    }

    #[test]
    fn range_raw_simple_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, meter, b"john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut store, meter, b"jim", &data2).unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = PEOPLE
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![
                (b"jim".to_vec(), data2.clone()),
                (b"john".to_vec(), data.clone())
            ]
        );

        // let's try to iterate over a range
        let all: PlusResult<Vec<_>> = PEOPLE
            .range_raw(
                &store,
                meter,
                Some(Bound::inclusive(b"j" as &[u8])),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![(b"jim".to_vec(), data2), (b"john".to_vec(), data.clone())]
        );

        // let's try to iterate over a more restrictive range
        let all: PlusResult<Vec<_>> = PEOPLE
            .range_raw(
                &store,
                meter,
                Some(Bound::inclusive(b"jo" as &[u8])),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(b"john".to_vec(), data)]);
    }

    #[test]
    fn range_simple_string_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, meter, b"john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut store, meter, b"jim", &data2).unwrap();

        let data3 = Data {
            name: "Ada".to_string(),
            age: 23,
        };
        PEOPLE.save(&mut store, meter, b"ada", &data3).unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = PEOPLE
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(
            all,
            vec![
                (b"ada".to_vec(), data3),
                (b"jim".to_vec(), data2.clone()),
                (b"john".to_vec(), data.clone())
            ]
        );

        // let's try to iterate over a range
        let all: PlusResult<Vec<_>> = PEOPLE
            .range(
                &store,
                meter,
                b"j".inclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![(b"jim".to_vec(), data2), (b"john".to_vec(), data.clone())]
        );

        // let's try to iterate over a more restrictive range
        let all: PlusResult<Vec<_>> = PEOPLE
            .range(
                &store,
                meter,
                b"jo".inclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(b"john".to_vec(), data)]);
    }

    #[test]
    fn range_key_broken_deserialization_errors() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_STR.save(&mut store, meter, "john", &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_STR.save(&mut store, meter, "jim", &data2).unwrap();

        let data3 = Data {
            name: "Ada".to_string(),
            age: 23,
        };
        PEOPLE_STR.save(&mut store, meter, "ada", &data3).unwrap();

        // let's iterate!
        let all: PlusResult<Vec<_>> = PEOPLE_STR
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(
            all,
            vec![
                ("ada".to_string(), data3.clone()),
                ("jim".to_string(), data2.clone()),
                ("john".to_string(), data.clone())
            ]
        );

        // Manually add a broken key (invalid utf-8)
        store
            .set(
                meter,
                &[
                    [0u8, PEOPLE_STR_KEY.len() as u8].as_slice(),
                    PEOPLE_STR_KEY.as_bytes(),
                    b"\xddim",
                ]
                .concat(),
                &to_json_binary(&data2).unwrap(),
            )
            .unwrap();

        // Let's try to iterate again!
        let all: PlusResult<Vec<_>> = PEOPLE_STR
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        assert!(all.is_err());
        assert!(matches!(
            all.unwrap_err(),
            PlusError::Std(StdError::InvalidUtf8 { .. })
        ));

        // And the same with keys()
        let all: PlusResult<Vec<_>> = PEOPLE_STR
            .keys(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        assert!(all.is_err());
        assert!(matches!(
            all.unwrap_err(),
            PlusError::Std(StdError::InvalidUtf8 { .. })
        ));

        // But range_raw still works
        let all: PlusResult<Vec<_>> = PEOPLE_STR
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();

        let all = all.unwrap();
        assert_eq!(4, all.len());
        assert_eq!(
            all,
            vec![
                (b"ada".to_vec(), data3.clone()),
                (b"jim".to_vec(), data2.clone()),
                (b"john".to_vec(), data.clone()),
                (b"\xddim".to_vec(), data2.clone()),
            ]
        );

        // And the same with keys_raw
        let all: GasResult<Vec<_>> = PEOPLE_STR
            .keys_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();

        assert_eq!(4, all.len());
        assert_eq!(
            all,
            vec![
                b"ada".to_vec(),
                b"jim".to_vec(),
                b"john".to_vec(),
                b"\xddim".to_vec(),
            ]
        );
    }

    #[test]
    fn range_simple_integer_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_ID.save(&mut store, meter, 1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_ID.save(&mut store, meter, 56, &data2).unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = PEOPLE_ID
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2.clone()), (1234, data.clone())]);

        // let's try to iterate over a range
        let all: PlusResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                meter,
                Some(Bound::inclusive(56u32)),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2), (1234, data.clone())]);

        // let's try to iterate over a more restrictive range
        let all: PlusResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                meter,
                Some(Bound::inclusive(57u32)),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(1234, data)]);
    }

    #[test]
    fn range_simple_integer_key_with_bounder_trait() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE_ID.save(&mut store, meter, 1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE_ID.save(&mut store, meter, 56, &data2).unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = PEOPLE_ID
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2.clone()), (1234, data.clone())]);

        // let's try to iterate over a range
        let all: PlusResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                meter,
                56u32.inclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(56, data2), (1234, data.clone())]);

        // let's try to iterate over a more restrictive range
        let all: PlusResult<Vec<_>> = PEOPLE_ID
            .range(
                &store,
                meter,
                57u32.inclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(1234, data)]);
    }

    #[test]
    fn range_simple_signed_integer_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        SIGNED_ID.save(&mut store, meter, -1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        SIGNED_ID.save(&mut store, meter, -56, &data2).unwrap();

        let data3 = Data {
            name: "Jules".to_string(),
            age: 55,
        };
        SIGNED_ID.save(&mut store, meter, 50, &data3).unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = SIGNED_ID
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        // order is correct
        assert_eq!(
            all,
            vec![(-1234, data), (-56, data2.clone()), (50, data3.clone())]
        );

        // let's try to iterate over a range
        let all: PlusResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                meter,
                Some(Bound::inclusive(-56i32)),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(-56, data2), (50, data3.clone())]);

        // let's try to iterate over a more restrictive range
        let all: PlusResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                meter,
                Some(Bound::inclusive(-55i32)),
                Some(Bound::inclusive(50i32)),
                Order::Descending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(50, data3)]);
    }

    #[test]
    fn range_simple_signed_integer_key_with_bounder_trait() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        SIGNED_ID.save(&mut store, meter, -1234, &data).unwrap();

        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        SIGNED_ID.save(&mut store, meter, -56, &data2).unwrap();

        let data3 = Data {
            name: "Jules".to_string(),
            age: 55,
        };
        SIGNED_ID.save(&mut store, meter, 50, &data3).unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = SIGNED_ID
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        // order is correct
        assert_eq!(
            all,
            vec![(-1234, data), (-56, data2.clone()), (50, data3.clone())]
        );

        // let's try to iterate over a range
        let all: PlusResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                meter,
                (-56i32).inclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(all, vec![(-56, data2), (50, data3.clone())]);

        // let's try to iterate over a more restrictive range
        let all: PlusResult<Vec<_>> = SIGNED_ID
            .range(
                &store,
                meter,
                (-55i32).inclusive_bound(),
                50i32.inclusive_bound(),
                Order::Descending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(50, data3)]);
    }

    #[test]
    fn range_raw_composite_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys, one under different owner
        ALLOWANCE
            .save(&mut store, meter, (b"owner", b"spender"), &1000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, meter, (b"owner", b"spender2"), &3000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, meter, (b"owner2", b"spender"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(
            all,
            vec![
                ((b"owner".to_vec(), b"spender".to_vec()).joined_key(), 1000),
                ((b"owner".to_vec(), b"spender2".to_vec()).joined_key(), 3000),
                ((b"owner2".to_vec(), b"spender".to_vec()).joined_key(), 5000),
            ]
        );

        // let's try to iterate over a prefix
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![(b"spender".to_vec(), 1000), (b"spender2".to_vec(), 3000)]
        );
    }

    #[test]
    fn range_composite_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys, one under different owner
        ALLOWANCE
            .save(&mut store, meter, (b"owner", b"spender"), &1000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, meter, (b"owner", b"spender2"), &3000)
            .unwrap();
        ALLOWANCE
            .save(&mut store, meter, (b"owner2", b"spender"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(
            all,
            vec![
                ((b"owner".to_vec(), b"spender".to_vec()), 1000),
                ((b"owner".to_vec(), b"spender2".to_vec()), 3000),
                ((b"owner2".to_vec(), b"spender".to_vec()), 5000)
            ]
        );

        // let's try to iterate over a prefix
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![(b"spender".to_vec(), 1000), (b"spender2".to_vec(), 3000),]
        );

        // let's try to iterate over a prefixed restricted inclusive range
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range(
                &store,
                meter,
                b"spender".inclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![(b"spender".to_vec(), 1000), (b"spender2".to_vec(), 3000),]
        );

        // let's try to iterate over a prefixed restricted exclusive range
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range(
                &store,
                meter,
                b"spender".exclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![(b"spender2".to_vec(), 3000),]);
    }

    #[test]
    fn range_raw_triple_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys, one under different owner
        TRIPLE
            .save(&mut store, meter, (b"owner", 9, "recipient"), &1000)
            .unwrap();
        TRIPLE
            .save(&mut store, meter, (b"owner", 9, "recipient2"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, meter, (b"owner", 10, "recipient3"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, meter, (b"owner2", 9, "recipient"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = TRIPLE
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(4, all.len());
        assert_eq!(
            all,
            vec![
                (
                    (b"owner".to_vec(), 9u8, b"recipient".to_vec()).joined_key(),
                    1000
                ),
                (
                    (b"owner".to_vec(), 9u8, b"recipient2".to_vec()).joined_key(),
                    3000
                ),
                (
                    (b"owner".to_vec(), 10u8, b"recipient3".to_vec()).joined_key(),
                    3000
                ),
                (
                    (b"owner2".to_vec(), 9u8, b"recipient".to_vec()).joined_key(),
                    5000
                )
            ]
        );

        // let's iterate over a prefix
        let all: PlusResult<Vec<_>> = TRIPLE
            .prefix((b"owner", 9))
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![
                (b"recipient".to_vec(), 1000),
                (b"recipient2".to_vec(), 3000)
            ]
        );

        // let's iterate over a sub prefix
        let all: PlusResult<Vec<_>> = TRIPLE
            .sub_prefix(b"owner")
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        // Use range() if you want key deserialization
        assert_eq!(
            all,
            vec![
                ((9u8, b"recipient".to_vec()).joined_key(), 1000),
                ((9u8, b"recipient2".to_vec()).joined_key(), 3000),
                ((10u8, b"recipient3".to_vec()).joined_key(), 3000)
            ]
        );
    }

    #[test]
    fn range_triple_key() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on three keys, one under different owner
        TRIPLE
            .save(&mut store, meter, (b"owner", 9u8, "recipient"), &1000)
            .unwrap();
        TRIPLE
            .save(&mut store, meter, (b"owner", 9u8, "recipient2"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, meter, (b"owner", 10u8, "recipient3"), &3000)
            .unwrap();
        TRIPLE
            .save(&mut store, meter, (b"owner2", 9u8, "recipient"), &5000)
            .unwrap();

        // let's try to iterate!
        let all: PlusResult<Vec<_>> = TRIPLE
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(4, all.len());
        assert_eq!(
            all,
            vec![
                ((b"owner".to_vec(), 9, "recipient".to_string()), 1000),
                ((b"owner".to_vec(), 9, "recipient2".to_string()), 3000),
                ((b"owner".to_vec(), 10, "recipient3".to_string()), 3000),
                ((b"owner2".to_vec(), 9, "recipient".to_string()), 5000)
            ]
        );

        // let's iterate over a sub_prefix
        let all: PlusResult<Vec<_>> = TRIPLE
            .sub_prefix(b"owner")
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(3, all.len());
        assert_eq!(
            all,
            vec![
                ((9, "recipient".to_string()), 1000),
                ((9, "recipient2".to_string()), 3000),
                ((10, "recipient3".to_string()), 3000),
            ]
        );

        // let's iterate over a prefix
        let all: PlusResult<Vec<_>> = TRIPLE
            .prefix((b"owner", 9))
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![
                ("recipient".to_string(), 1000),
                ("recipient2".to_string(), 3000),
            ]
        );

        // let's try to iterate over a prefixed restricted inclusive range
        let all: PlusResult<Vec<_>> = TRIPLE
            .prefix((b"owner", 9))
            .range(
                &store,
                meter,
                "recipient".inclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(2, all.len());
        assert_eq!(
            all,
            vec![
                ("recipient".to_string(), 1000),
                ("recipient2".to_string(), 3000),
            ]
        );

        // let's try to iterate over a prefixed restricted exclusive range
        let all: PlusResult<Vec<_>> = TRIPLE
            .prefix((b"owner", 9))
            .range(
                &store,
                meter,
                "recipient".exclusive_bound(),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        let all = all.unwrap();
        assert_eq!(1, all.len());
        assert_eq!(all, vec![("recipient2".to_string(), 3000),]);
    }

    #[test]
    fn basic_update() {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        let add_ten = |a: Option<u64>| -> PlusResult<_> { Ok(a.unwrap_or_default() + 10) };

        // save and load on three keys, one under different owner
        let key: (&[u8], &[u8]) = (b"owner", b"spender");
        ALLOWANCE.update(&mut store, meter, key, add_ten).unwrap();
        let twenty = ALLOWANCE.update(&mut store, meter, key, add_ten).unwrap();
        assert_eq!(20, twenty);
        let loaded = ALLOWANCE.load(&store, meter, key).unwrap();
        assert_eq!(20, loaded);
    }

    #[test]
    fn readme_works() -> PlusResult<()> {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        let data = Data {
            name: "John".to_string(),
            age: 32,
        };

        // load and save with extra key argument
        let empty = PEOPLE.may_load(&store, meter, b"john")?;
        assert_eq!(None, empty);
        PEOPLE.save(&mut store, meter, b"john", &data)?;
        let loaded = PEOPLE.load(&store, meter, b"john")?;
        assert_eq!(data, loaded);

        // nothing on another key
        let missing = PEOPLE.may_load(&store, meter, b"jack")?;
        assert_eq!(None, missing);

        // update function for new or existing keys
        let birthday = |d: Option<Data>| -> PlusResult<Data> {
            match d {
                Some(one) => Ok(Data {
                    name: one.name,
                    age: one.age + 1,
                }),
                None => Ok(Data {
                    name: "Newborn".to_string(),
                    age: 0,
                }),
            }
        };

        let old_john = PEOPLE.update(&mut store, meter, b"john", birthday)?;
        assert_eq!(33, old_john.age);
        assert_eq!("John", old_john.name.as_str());

        let new_jack = PEOPLE.update(&mut store, meter, b"jack", birthday)?;
        assert_eq!(0, new_jack.age);
        assert_eq!("Newborn", new_jack.name.as_str());

        // update also changes the store
        assert_eq!(old_john, PEOPLE.load(&store, meter, b"john")?);
        assert_eq!(new_jack, PEOPLE.load(&store, meter, b"jack")?);

        // removing leaves us empty
        PEOPLE.remove(&mut store, meter, b"john")?;
        let empty = PEOPLE.may_load(&store, meter, b"john")?;
        assert_eq!(None, empty);

        Ok(())
    }

    #[test]
    fn readme_works_composite_keys() -> PlusResult<()> {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on a composite key
        let empty = ALLOWANCE.may_load(&store, meter, (b"owner", b"spender"))?;
        assert_eq!(None, empty);
        ALLOWANCE.save(&mut store, meter, (b"owner", b"spender"), &777)?;
        let loaded = ALLOWANCE.load(&store, meter, (b"owner", b"spender"))?;
        assert_eq!(777, loaded);

        // doesn't appear under other key (even if a concat would be the same)
        let different = ALLOWANCE
            .may_load(&store, meter, (b"owners", b"pender"))
            .unwrap();
        assert_eq!(None, different);

        // simple update
        ALLOWANCE.update(
            &mut store,
            meter,
            (b"owner", b"spender"),
            |v| -> PlusResult<u64> { Ok(v.unwrap_or_default() + 222) },
        )?;
        let loaded = ALLOWANCE.load(&store, meter, (b"owner", b"spender"))?;
        assert_eq!(999, loaded);

        Ok(())
    }

    #[test]
    fn readme_works_with_path() -> PlusResult<()> {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        let data = Data {
            name: "John".to_string(),
            age: 32,
        };

        // create a Path one time to use below
        let john = PEOPLE.key(b"john");

        // Use this just like an Item above
        let empty = john.may_load(&store, meter)?;
        assert_eq!(None, empty);
        john.save(&mut store, meter, &data)?;
        let loaded = john.load(&store, meter)?;
        assert_eq!(data, loaded);
        john.remove(&mut store, meter).unwrap();
        let empty = john.may_load(&store, meter)?;
        assert_eq!(None, empty);

        // same for composite keys, just use both parts in key()
        let allow = ALLOWANCE.key((b"owner", b"spender"));
        allow.save(&mut store, meter, &1234)?;
        let loaded = allow.load(&store, meter)?;
        assert_eq!(1234, loaded);
        allow.update(&mut store, meter, |x| -> PlusResult<u64> {
            Ok(x.unwrap_or_default() * 2)
        })?;
        let loaded = allow.load(&store, meter)?;
        assert_eq!(2468, loaded);

        Ok(())
    }

    #[test]
    fn readme_with_range_raw() -> PlusResult<()> {
        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        // save and load on two keys
        let data = Data {
            name: "John".to_string(),
            age: 32,
        };
        PEOPLE.save(&mut store, meter, b"john", &data)?;
        let data2 = Data {
            name: "Jim".to_string(),
            age: 44,
        };
        PEOPLE.save(&mut store, meter, b"jim", &data2)?;

        // iterate over them all
        let all: PlusResult<Vec<_>> = PEOPLE
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        assert_eq!(
            all?,
            vec![(b"jim".to_vec(), data2), (b"john".to_vec(), data.clone())]
        );

        // or just show what is after jim
        let all: PlusResult<Vec<_>> = PEOPLE
            .range_raw(
                &store,
                meter,
                Some(Bound::exclusive(b"jim" as &[u8])),
                None,
                Order::Ascending,
            )
            .unwrap()
            .collect();
        assert_eq!(all?, vec![(b"john".to_vec(), data)]);

        // save and load on three keys, one under different owner
        ALLOWANCE.save(&mut store, meter, (b"owner", b"spender"), &1000)?;
        ALLOWANCE.save(&mut store, meter, (b"owner", b"spender2"), &3000)?;
        ALLOWANCE.save(&mut store, meter, (b"owner2", b"spender"), &5000)?;

        // get all under one key
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        assert_eq!(
            all?,
            vec![(b"spender".to_vec(), 1000), (b"spender2".to_vec(), 3000)]
        );

        // Or ranges between two items (even reverse)
        let all: PlusResult<Vec<_>> = ALLOWANCE
            .prefix(b"owner")
            .range_raw(
                &store,
                meter,
                Some(Bound::exclusive(b"spender1" as &[u8])),
                Some(Bound::inclusive(b"spender2" as &[u8])),
                Order::Descending,
            )
            .unwrap()
            .collect();
        assert_eq!(all?, vec![(b"spender2".to_vec(), 3000)]);

        Ok(())
    }

    #[test]
    fn prefixed_range_raw_works() {
        // this is designed to look as much like a secondary index as possible
        // we want to query over a range of u32 for the first key and all subkeys
        const AGES: Map<(u32, Vec<u8>), u64> = Map::new("ages");

        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        AGES.save(&mut store, meter, (2, vec![1, 2, 3]), &123)
            .unwrap();
        AGES.save(&mut store, meter, (3, vec![4, 5, 6]), &456)
            .unwrap();
        AGES.save(&mut store, meter, (5, vec![7, 8, 9]), &789)
            .unwrap();
        AGES.save(&mut store, meter, (5, vec![9, 8, 7]), &987)
            .unwrap();
        AGES.save(&mut store, meter, (7, vec![20, 21, 22]), &2002)
            .unwrap();
        AGES.save(&mut store, meter, (8, vec![23, 24, 25]), &2332)
            .unwrap();

        // typical range under one prefix as a control
        let fives = AGES
            .prefix(5)
            .range_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(fives.len(), 2);
        assert_eq!(fives, vec![(vec![7, 8, 9], 789), (vec![9, 8, 7], 987)]);

        let keys: GasResult<Vec<_>> = AGES
            .keys_raw(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let keys = keys.unwrap();
        println!("keys: {:?}", keys);

        // using inclusive bounds both sides
        let include = AGES
            .prefix_range_raw(
                &store,
                meter,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(7u32)),
                Order::Ascending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 4);
        assert_eq!(include, vec![456, 789, 987, 2002]);

        // using exclusive bounds both sides
        let exclude = AGES
            .prefix_range_raw(
                &store,
                meter,
                Some(PrefixBound::exclusive(3u32)),
                Some(PrefixBound::exclusive(7u32)),
                Order::Ascending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(exclude.len(), 2);
        assert_eq!(exclude, vec![789, 987]);

        // using inclusive in descending
        let include = AGES
            .prefix_range_raw(
                &store,
                meter,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(5u32)),
                Order::Descending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 3);
        assert_eq!(include, vec![987, 789, 456]);

        // using exclusive in descending
        let include = AGES
            .prefix_range_raw(
                &store,
                meter,
                Some(PrefixBound::exclusive(2u32)),
                Some(PrefixBound::exclusive(5u32)),
                Order::Descending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 1);
        assert_eq!(include, vec![456]);
    }

    #[test]
    fn prefixed_range_works() {
        // this is designed to look as much like a secondary index as possible
        // we want to query over a range of u32 for the first key and all subkeys
        const AGES: Map<(u32, &str), u64> = Map::new("ages");

        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        AGES.save(&mut store, meter, (2, "123"), &123).unwrap();
        AGES.save(&mut store, meter, (3, "456"), &456).unwrap();
        AGES.save(&mut store, meter, (5, "789"), &789).unwrap();
        AGES.save(&mut store, meter, (5, "987"), &987).unwrap();
        AGES.save(&mut store, meter, (7, "202122"), &2002).unwrap();
        AGES.save(&mut store, meter, (8, "232425"), &2332).unwrap();

        // typical range under one prefix as a control
        let fives = AGES
            .prefix(5)
            .range(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(fives.len(), 2);
        assert_eq!(
            fives,
            vec![("789".to_string(), 789), ("987".to_string(), 987)]
        );

        let keys: PlusResult<Vec<_>> = AGES
            .keys(&store, meter, None, None, Order::Ascending)
            .unwrap()
            .collect();
        let keys = keys.unwrap();
        println!("keys: {:?}", keys);

        // using inclusive bounds both sides
        let include = AGES
            .prefix_range(
                &store,
                meter,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(7u32)),
                Order::Ascending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 4);
        assert_eq!(include, vec![456, 789, 987, 2002]);

        // using exclusive bounds both sides
        let exclude = AGES
            .prefix_range(
                &store,
                meter,
                Some(PrefixBound::exclusive(3u32)),
                Some(PrefixBound::exclusive(7u32)),
                Order::Ascending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(exclude.len(), 2);
        assert_eq!(exclude, vec![789, 987]);

        // using inclusive in descending
        let include = AGES
            .prefix_range(
                &store,
                meter,
                Some(PrefixBound::inclusive(3u32)),
                Some(PrefixBound::inclusive(5u32)),
                Order::Descending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 3);
        assert_eq!(include, vec![987, 789, 456]);

        // using exclusive in descending
        let include = AGES
            .prefix_range(
                &store,
                meter,
                Some(PrefixBound::exclusive(2u32)),
                Some(PrefixBound::exclusive(5u32)),
                Order::Descending,
            )
            .unwrap()
            .map(|r| r.map(|(_, v)| v))
            .collect::<PlusResult<Vec<_>>>()
            .unwrap();
        assert_eq!(include.len(), 1);
        assert_eq!(include, vec![456]);
    }

    #[test]
    fn clear_works() {
        const TEST_MAP: Map<&str, u32> = Map::new("test_map");

        let store = MemoryStore::new();
        let mut storage = store.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        TEST_MAP.save(&mut storage, meter, "key0", &0u32).unwrap();
        TEST_MAP.save(&mut storage, meter, "key1", &1u32).unwrap();
        TEST_MAP.save(&mut storage, meter, "key2", &2u32).unwrap();
        TEST_MAP.save(&mut storage, meter, "key3", &3u32).unwrap();
        TEST_MAP.save(&mut storage, meter, "key4", &4u32).unwrap();

        TEST_MAP.clear(&mut storage, meter).unwrap();

        assert!(!TEST_MAP.has(&storage, meter, "key0").unwrap());
        assert!(!TEST_MAP.has(&storage, meter, "key1").unwrap());
        assert!(!TEST_MAP.has(&storage, meter, "key2").unwrap());
        assert!(!TEST_MAP.has(&storage, meter, "key3").unwrap());
        assert!(!TEST_MAP.has(&storage, meter, "key4").unwrap());
    }

    #[test]
    fn is_empty_works() {
        const TEST_MAP: Map<&str, u32> = Map::new("test_map");

        let storage = MemoryStore::new();
        let mut store = storage.writer();
        let gas_meter = GasMeter::infinite();
        let meter = &gas_meter;

        assert!(TEST_MAP.is_empty(&store, meter).unwrap());

        TEST_MAP.save(&mut store, meter, "key1", &1u32).unwrap();
        TEST_MAP.save(&mut store, meter, "key2", &2u32).unwrap();

        assert!(!TEST_MAP.is_empty(&store, meter).unwrap());
    }
}
