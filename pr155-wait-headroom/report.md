# Upload queue capacity and pod memory headroom

PR #155, commit `ec32558c25528596b6cd7b6cd9e2d0c9cfe712cb`. Two ready gateway pods with eight active upload slots, two CPUs, 1 GiB RAM and a 512 MiB serialized-body budget each. The upload queue deadline was ten seconds. The waiter count below is per pod.

## Verified requests and latency

Each 240 req/s burst workload sent 240 uploads in the first 250 ms of each second, for an instantaneous offered rate of 960 req/s. The steady workload sent 480 req/s evenly. The verified count includes completions during drain. Within two seconds measures from scheduled arrival. Successful p95 and p99 cover verified responses.

| Workload | Waiters | Runs × seconds | Verified / offered | Within 2 s | p95 / p99 (s) | Longest drain (s) | HTTP 503 / transport / other failures |
|---|---:|---:|---:|---:|---:|---:|---:|
| Repeated bursts | 16 | 2 × 20 | 5,364 / 9,600 (55.875%) | 5,364 | 1.03 / 1.21 | 0.97 | 4,216 / 20 / 0 |
| Repeated bursts | 64 | 1 × 20 | 4,221 / 4,800 (87.938%) | 4,221 | 1.32 / 1.52 | 1.25 | 578 / 1 / 0 |
| Repeated bursts | 128 | 2 × 20 | 9,575 / 9,600 (99.740%) | 9,572 | 1.55 / 1.76 | 1.34 | 25 / 0 / 0 |
| Repeated bursts | 128 | 1 × 60 | 14,361 / 14,400 (99.729%) | 14,358 | 1.51 / 1.74 | 1.44 | 39 / 0 / 0 |
| Repeated bursts | 192 | 2 × 20 | 9,600 / 9,600 (100.000%) | 9,576 | 1.60 / 1.82 | 1.35 | 0 / 0 / 0 |
| Repeated bursts | 192 | 1 × 60 | 14,400 / 14,400 (100.000%) | 14,376 | 1.53 / 1.77 | 1.55 | 0 / 0 / 0 |
| Repeated bursts | 256 | 2 × 20 | 9,600 / 9,600 (100.000%) | 9,565 | 1.59 / 1.79 | 1.35 | 0 / 0 / 0 |
| Repeated bursts | 256 | 1 × 60 | 14,399 / 14,400 (99.993%) | 14,398 | 1.52 / 1.78 | 1.43 | 0 / 0 / 1 |
| Steady traffic | 16 | 2 × 20 | 11,440 / 19,200 (59.583%) | 11,440 | 1.07 / 1.22 | 1.06 | 7,729 / 31 / 0 |
| Steady traffic | 256 | 1 × 20 | 6,191 / 9,600 (64.490%) | 1,007 | 3.04 / 3.45 | 2.59 | 3,386 / 23 / 0 |
| Steady traffic | 1,024 | 1 × 20 | 7,787 / 9,600 (81.115%) | 968 | 8.72 / 9.01 | 8.67 | 1,806 / 7 / 0 |
| Steady traffic | 2,048 | 1 × 20 | 6,737 / 9,600 (70.177%) | 978 | 10.82 / 10.94 | 13.02 | 2,863 / 0 / 0 |
| Steady traffic | 4,096 | 1 × 20 | 6,992 / 9,600 (72.833%) | 956 | 10.81 / 10.93 | 13.18 | 2,608 / 0 / 0 |
| Steady traffic | 4,096 | 1 × 60 | 12,742 / 28,800 (44.243%) | 1,298 | 10.87 / 11.39 | 11.14 | 16,057 / 0 / 1 |

## Memory and blocking limits

Per-pod headroom is 1,024 MiB minus the highest kernel-reported cgroup memory.peak recorded across either gateway pod and all trials in that row, including warmup and drain. RSS and socket peaks are sampled approximately every 0.2 seconds. They may occur on different pods or at different times. Cgroup totals already account for socket memory. The sampled queue occupancy is a lower bound on the actual maximum.

| Workload | Waiters | Runs × seconds | Peak pod / remaining MiB | Pod RSS / socket peaks MiB | Sampled max waiters | Queue full / timeout / byte denials | Restarts |
|---|---:|---:|---:|---:|---:|---:|---:|
| Repeated bursts | 16 | 2 × 20 | 329.8 / 694.2 | 336.0 / 25.1 | 16 | 4,236 / 0 / 0 | 0 |
| Repeated bursts | 64 | 1 × 20 | 331.1 / 692.9 | 306.0 / 34.8 | 64 | 579 / 0 / 0 | 0 |
| Repeated bursts | 128 | 2 × 20 | 363.1 / 660.9 | 332.3 / 33.7 | 118 | 25 / 0 / 0 | 0 |
| Repeated bursts | 128 | 1 × 60 | 374.5 / 649.5 | 358.0 / 55.7 | 117 | 39 / 0 / 0 | 0 |
| Repeated bursts | 192 | 2 × 20 | 360.6 / 663.4 | 357.2 / 41.9 | 138 | 0 / 0 / 0 | 0 |
| Repeated bursts | 192 | 1 × 60 | 387.6 / 636.4 | 336.6 / 60.9 | 145 | 0 / 0 / 0 | 0 |
| Repeated bursts | 256 | 2 × 20 | 348.8 / 675.2 | 333.2 / 33.9 | 139 | 0 / 0 / 0 | 0 |
| Repeated bursts | 256 | 1 × 60 | 362.4 / 661.6 | 340.1 / 77.8 | 143 | 0 / 0 / 0 | 0 |
| Steady traffic | 16 | 2 × 20 | 338.0 / 686.0 | 311.8 / 22.7 | 16 | 7,760 / 0 / 0 | 0 |
| Steady traffic | 256 | 1 × 20 | 424.8 / 599.2 | 393.5 / 48.0 | 256 | 3,409 / 0 / 0 | 0 |
| Steady traffic | 1,024 | 1 × 20 | 527.0 / 497.0 | 466.8 / 102.1 | 1,024 | 1,813 / 0 / 0 | 0 |
| Steady traffic | 2,048 | 1 × 20 | 791.6 / 232.4 | 646.6 / 141.7 | 2,048 | 598 / 2,265 / 0 | 0 |
| Steady traffic | 4,096 | 1 × 20 | 961.9 / 62.1 | 838.9 / 160.7 | 2,350 | 0 / 2,608 / 0 | 0 |
| Steady traffic | 4,096 | 1 × 60 | 887.7 / 136.3 | 656.8 / 164.3 | 2,429 | 0 / 16,057 / 0 | 0 |

## Method

The same native arm64 release image served every run (Docker image ID `sha256:30ed0b27eee47b7ed2375de53dd52545c6893e4eebe36a19b1c6aa1862b82782`). The host was an Apple M1 Pro with a 10-CPU Docker VM. Each new trial started fresh gateway pods, awaited readiness and warmed the routes. The 16-waiter controls come from the previous same-image experiment. Input class sequences and payload digests were checked across matched workload, duration and seed. We used three mock service replicas, real PostgreSQL and Redis, four independently paced client pods, HTTP/1.1 keep-alive and equal distribution across the two gateways.

The registered-service JSON route received synthetic chat-history, tool-call and retrieval bodies spanning 8 KiB to 100 MiB. Three-quarters of requests were 128 KiB or smaller. The upstream delayed reads for 250 ms and responses for 500 ms. A verified request returned HTTP 200 with the exact upstream byte count and SHA-256, and confirmation of the expected service credential. Client-side write errors count as failures even when the gateway recorded an overload denial. The client deadline was 30 seconds.

The drain figure starts at the final scheduled arrival. The trials measure a local gateway deployment with this distribution, provider delay and request policy. A full success count describes the observed finite window. Long-running production traffic can have different burst shapes, processing rates and deadlines.

[Per-trial data](trial-results.csv) · [Aggregate CSV](summary.csv) · [Completions and outstanding requests by ten-second window](steady-windows.csv) · [Raw evidence checksums](checksums.sha256) · [Build and machine](../pr155-performance/provenance.json)

[Paired LiteLLM v1.110.0 benchmark](head-to-head/report.md)
