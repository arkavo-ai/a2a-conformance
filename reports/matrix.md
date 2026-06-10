# A2A interop matrix

Cell format: `pass/applicable` (skip = harness reported unsupported; n/a = excluded by appliesTo). Self-pairs are sanity checks, not interop evidence.

## core

| client \ server | arkavo-swift | rust-a2a | tolgaki-swift |
|---|---|---|---|
| **arkavo-swift** | [✅ 6/6](cells/arkavo-swift--arkavo-swift.md) (self) | [✅ 6/6](cells/arkavo-swift--rust-a2a.md) | [❌ 2/4 +2skip](cells/arkavo-swift--tolgaki-swift.md) |
| **rust-a2a** | [✅ 6/6](cells/rust-a2a--arkavo-swift.md) | [✅ 6/6](cells/rust-a2a--rust-a2a.md) (self) | [❌ 0/4 +2skip](cells/rust-a2a--tolgaki-swift.md) |
| **tolgaki-swift** | [✅ 6/6](cells/tolgaki-swift--arkavo-swift.md) | [❌ 4/6](cells/tolgaki-swift--rust-a2a.md) | [✅ 4/4 +2skip](cells/tolgaki-swift--tolgaki-swift.md) (self) |

## streaming

| client \ server | arkavo-swift | rust-a2a | tolgaki-swift |
|---|---|---|---|
| **arkavo-swift** | [✅ 4/4](cells/arkavo-swift--arkavo-swift.md) (self) | [✅ 4/4](cells/arkavo-swift--rust-a2a.md) | [✅ 4/4](cells/arkavo-swift--tolgaki-swift.md) |
| **rust-a2a** | [✅ 4/4](cells/rust-a2a--arkavo-swift.md) | [✅ 4/4](cells/rust-a2a--rust-a2a.md) (self) | [❌ 0/4](cells/rust-a2a--tolgaki-swift.md) |
| **tolgaki-swift** | [✅ 4/4](cells/tolgaki-swift--arkavo-swift.md) | [❌ 0/4](cells/tolgaki-swift--rust-a2a.md) | [✅ 4/4](cells/tolgaki-swift--tolgaki-swift.md) (self) |

## errors

| client \ server | arkavo-swift | rust-a2a | tolgaki-swift |
|---|---|---|---|
| **arkavo-swift** | [✅ 11/11](cells/arkavo-swift--arkavo-swift.md) (self) | [❌ 10/11](cells/arkavo-swift--rust-a2a.md) | [❌ 7/9 +2skip](cells/arkavo-swift--tolgaki-swift.md) |
| **rust-a2a** | [✅ 11/11](cells/rust-a2a--arkavo-swift.md) | [❌ 10/11](cells/rust-a2a--rust-a2a.md) (self) | [❌ 7/9 +2skip](cells/rust-a2a--tolgaki-swift.md) |
| **tolgaki-swift** | [❌ 10/11](cells/tolgaki-swift--arkavo-swift.md) | [❌ 5/11](cells/tolgaki-swift--rust-a2a.md) | [❌ 8/9 +2skip](cells/tolgaki-swift--tolgaki-swift.md) (self) |

## discovery

| client \ server | arkavo-swift | rust-a2a | tolgaki-swift |
|---|---|---|---|
| **arkavo-swift** | [✅ 3/3](cells/arkavo-swift--arkavo-swift.md) (self) | [✅ 3/3](cells/arkavo-swift--rust-a2a.md) | [✅ 2/2 +1skip](cells/arkavo-swift--tolgaki-swift.md) |
| **rust-a2a** | [❌ 2/3](cells/rust-a2a--arkavo-swift.md) | [❌ 2/3](cells/rust-a2a--rust-a2a.md) (self) | [✅ 2/2 +1skip](cells/rust-a2a--tolgaki-swift.md) |
| **tolgaki-swift** | [❌ 2/3](cells/tolgaki-swift--arkavo-swift.md) | [❌ 1/3](cells/tolgaki-swift--rust-a2a.md) | [❌ 1/2 +1skip](cells/tolgaki-swift--tolgaki-swift.md) (self) |

## edge

| client \ server | arkavo-swift | rust-a2a | tolgaki-swift |
|---|---|---|---|
| **arkavo-swift** | [✅ 5/5](cells/arkavo-swift--arkavo-swift.md) (self) | [✅ 3/3 +2skip](cells/arkavo-swift--rust-a2a.md) | [❌ 3/4 +1skip](cells/arkavo-swift--tolgaki-swift.md) |
| **rust-a2a** | [❌ 4/5](cells/rust-a2a--arkavo-swift.md) | [✅ 3/3 +2skip](cells/rust-a2a--rust-a2a.md) (self) | [❌ 1/4 +1skip](cells/rust-a2a--tolgaki-swift.md) |
| **tolgaki-swift** | [✅ 5/5](cells/tolgaki-swift--arkavo-swift.md) | [❌ 2/3 +2skip](cells/tolgaki-swift--rust-a2a.md) | [✅ 4/4 +1skip](cells/tolgaki-swift--tolgaki-swift.md) (self) |

