# Identity and hashing

**Status:** provisional. The language rules are in
[Equality and ordering](../language/expressions.md#equality-and-ordering) and
[Map keys](../language/collections.md#keys). This page fixes the invariants
the VM must keep, describes the simplest scheme that keeps them, and lists
faster schemes as experiments to measure later.

## Invariants

Every representation must keep these, whatever optimizations it uses:

1. **Equal values hash equally.** If `a == b`, then `hash(a) == hash(b)`.
2. **A hash never changes while its value is alive**, even if the collector
   moves the object.
3. **String hashes depend only on content.** Two equal strings hash the same
   whether they came from literals, from interpolation, from the host, or
   from separately compiled chunks.
4. **No observable difference.** Which scheme is in use never changes what a
   script can observe. Iteration follows insertion order, so the hash
   function itself is never visible.

## First implementation

The simplest scheme that keeps the invariants:

| Value | `==` | Hash |
|---|---|---|
| Integers, `bool`, `char`, payload-less enum cases | Compare the bits or tag | A cheap mix of the bits |
| Floats | IEEE comparison | Mix of the bits, after normalizing `-0.0` |
| Objects (class instances, arrays, maps, tasks, channels, closures) | Compare pointers | Mix of the address |
| Functions with no captures | Compare function indexes | Mix of the index |
| Strings, `bytes` | Same pointer → equal; different lengths or different cached hashes → not equal; otherwise compare bytes | **Content hash**, computed on first use and cached in the string object |
| Structs, `[N]T`, enums with payloads | Generated comparison (below) | Generated field-by-field hash |

- **The first collector is non-moving**, so an object's address is stable and
  can serve directly as its identity hash. That keeps invariant 2 with no
  extra state.
- **String literals are interned in the runtime's intern table** when a chunk
  is loaded, not in a per-compilation table. Equal literals in separately
  compiled chunks are therefore the same object, and compare with the
  pointer fast path. Interning is only a speed-up: equality and hashing stay
  correct for strings that were never interned, because both use content
  (invariant 3).
- The intern table holds strings weakly, so an unused literal can be freed.

## Generated equality and hashing

The compiler knows every static type, and `==` never runs user code, so it
generates a comparison and a hash per type, only for types that are actually
compared or used as keys:

| Type | Generated code |
|---|---|
| Enum with payloads | Tags first; payloads only when the tags match |
| Small struct or `[N]T` | Straight-line compares; integer, bool, and pointer fields a machine word at a time; cheap fields first so a mismatch exits early; floats individually, for IEEE rules |
| Large struct or `[N]T` | A loop or a shared helper, above a size threshold, so code size doesn't grow with the size of the data. See [specialization limits](../design/generics.md#implementation-specialization) |
| `?T` | Presence first, then the inner value |
| `equals()` on collections | Same object or different lengths exits immediately; then element by element |

## Map representations

The key type is known statically. The first implementation uses one
insertion-ordered hash table (a dense entry array plus an index) for every key
type. These specializations are planned, to be adopted only when measurements
show a gain:

| Key type | Specialized representation |
|---|---|
| Payload-less enum with N cases | A fixed array of N slots, indexed by tag |
| `bool` | Two slots |
| Any type, small maps | No index: a linear scan of the entry array up to about 8 entries |
| Constant keys at a lookup site | The key's hash computed at compile time (it depends only on content), and an inline cache of the entry's position guarded by the map's version counter |

Only float-keyed maps pay for `-0.0` normalization and the `NaN` check, since
the key type is known statically.

## Experiments

These are candidates for later, each to be measured against the first
implementation. None may break the invariants.

- **A moving collector.** Addresses would no longer be stable, so objects
  would need a hash state in their header: *never hashed*, *hashed*, or
  *moved after hashing*. The first hash is derived from the current address;
  later hashes before any move recompute it from the same address; when the
  collector moves a hashed object, it stores that original hash in an extra
  word on the moved copy, and later hashes read the stored word. Objects never
  hashed pay nothing.
- **Eager interning of short strings**, as Lua does for strings up to about
  40 bytes. Equal short strings would always be the same object, making their
  comparison a pointer compare, at the cost of an intern lookup for every
  short string created.
- **Shape sharing** for generic instantiations; see
  [Generics](../design/generics.md#implementation-specialization).
