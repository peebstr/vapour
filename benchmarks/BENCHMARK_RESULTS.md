# Vapour Benchmark Report

**Generated:** 2026-11-02T15:19:41Z  
**Architecture:** x86_64  
**Kernel:** 7.3.1-1-cachyos  
**Mode:** live (live measurement)  

---

## 1. Summary

| Metric | Before reclaim | After reclaim | Delta |
| :--- | :---: | :---: | :---: |
| **Steam + CEF RSS** | 1213 MB | 44 MB | -1169 MB (96.4%) |
| **Steam swap / zRAM** | — | 946 MB | +946 MB |
| **Host zRAM compression ratio** | — | 4.29x | — |

RSS moved to swap still costs its compressed size in zRAM; the net saving is lower than the RSS delta.

---

## 2. Details

- **Target process profiled:** PID `77380`
- **RSS before reclaim:** `1213 MB` (Core: `108 MB`, CEF webhelpers: `1104 MB`)
- **RSS after reclaim:** `44 MB`
- **Moved to swap/zRAM:** `946 MB`
- **`vapour status --json` runtime:** `4 ms` (average of 5 runs; this is CLI overhead, not game launch time)
- **PSI memory some avg10:** `0.00%`
- **Page faults of the profiled process:** `28950` minor, `1411` major
