# Slay3r Storage Plus

This is a port of the great work done in cw-storage-plus. Unfortunately, that work
was tied to `cosmwasm_std::Storage`, while we need `plusar_storage::Storage` support,
along with passing in a `GasMeter` and returning `GasError`.

All logic and functions are the same, args and results have been modified to support
these changes for the different storage interface.

This means the slay3r app modules can use a `cw-storage-plus`-like interface to manage
the internal state, which should be familiar to cosmwasm devs (and maybe allow easier porting
between slay3r modules and cosmwasm contracts)

## TODO

Re-write PrefixedStorage to use the tested code from plus.
Add some high-level tests
(This is a subset of plus::Prefix with a different API, no use for two versions of prefix code)

Add some tests on memory store for basic flows (eg no deadlock in commit)

Add some tests on lmdb for some basic flows

## Status

Ported:
* Item
* Map
* Prefix

Not yet ported (to do if/when needed):
* Deque
* IndexedMap
* SnapshotMap


## Open Questions

Do we stick with JSON encoding or use protobuf instead?