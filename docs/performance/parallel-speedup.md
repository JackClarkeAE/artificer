# Deterministic parallel-compute baseline

- Date: 2026-08-01
- Machine visible to the benchmark: 10 logical processors
- Build: Rust release profile
- Method: five warmed samples per task; table reports the median
- Comparison: the same ordered algorithms through a forced one-thread pool and a ten-thread pool

| Heavy task | Workload | 1 thread | 10 threads | Speed-up | Time saved |
|---|---:|---:|---:|---:|---:|
| Dense sketch arrangement, end to end | 224 curves / 12,544 crossings | 1988.008 ms | 1994.855 ms | 1.00x | -0.3% |
| B-rep validation batch | 4,096 immutable bodies | 32.006 ms | 8.628 ms | 3.71x | 73.0% |
| Display tessellation batch | 4,096 immutable bodies | 16.311 ms | 7.280 ms | 2.24x | 55.4% |
| Viewport projection mathematics | 3,000,000 vertices | 3.517 ms | 1.484 ms | 2.37x | 57.8% |

The sketch result is intentionally retained rather than hidden: analytic pair
evaluation is now parallel, but canonical arrangement assembly dominates this
particular dense line-grid workload and must remain ordered. The production
threshold keeps small/cheap batches serial. A later arrangement milestone can
parallelise immutable fragment preparation before the serial graph publication
stage; this baseline provides the number that change must beat.

Validation, tessellation, and viewport results preserve input order. Topology
ID allocation, canonical sorting and hashing, history publication, and document
mutation remain serial. Cancellable preview jobs may run out of order, but only
the latest non-cancelled generation is publishable.

Reproduce from the workspace root with:

```console
cargo run --release -p artificer-compute-bench
```
