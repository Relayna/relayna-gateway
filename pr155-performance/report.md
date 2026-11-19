# In-proxy waiting under load

Gateway commit `ec32558c25528596b6cd7b6cd9e2d0c9cfe712cb` from PR #155. Both configurations used the same release build (Docker image ID `sha256:30ed0b27eee47b7ed2375de53dd52545c6893e4eebe36a19b1c6aa1862b82782`). Trials ran on 19 November 2026 on an Apple M1 Pro, native arm64 kind, and a 10-CPU Docker VM.

## Client outcomes

Latency runs from scheduled arrival to verified response. Successful-request percentiles exclude rejected uploads. The two-second count uses every offered request as the denominator. Verified/s includes the time to drain after the last arrival.

| Load | Waiters per pod | Runs | Verified / offered | Within 2 s | 503 | 429 | Transport errors | Success p50 / p95 / p99 (ms) | Verified/s |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Steady 120/s | 0 | 1 | 4,667 / 4,800 | 4,667 | 133 | 0 | 0 | 757 / 857 / 931 | 114.5 |
| Steady 120/s | 16 | 1 | 4,800 / 4,800 | 4,800 | 0 | 0 | 0 | 758 / 868 / 931 | 117.8 |
| Bursts, 240/s average | 0 | 2 | 3,513 / 9,600 | 3,513 | 6,087 | 0 | 0 | 779 / 936 / 1,031 | 87.2 |
| Bursts, 240/s average | 16 | 2 | 5,364 / 9,600 | 5,364 | 4,216 | 0 | 20 | 811 / 1,026 / 1,211 | 132.6 |
| Steady 480/s | 0 | 2 | 8,989 / 19,200 | 8,989 | 10,211 | 0 | 0 | 787 / 974 / 1,163 | 215.3 |
| Steady 480/s | 16 | 2 | 11,440 / 19,200 | 11,440 | 7,729 | 0 | 31 | 853 / 1,069 / 1,219 | 272.1 |

With sixteen waiters, the two pulse trials recorded 4,236 queue-full denials. Clients received 4,216 HTTP 503 responses and saw 20 write errors. At 480/s the corresponding numbers were 7,760 denials, 7,729 HTTP 503s and 31 write errors. No trial recorded a queue timeout. The test counts transport errors separately from HTTP responses.

## Memory

Every approximately 0.2 s, the node sampled process RSS, kernel TCP socket allocation from cgroup v2 memory.stat (sock), and total cgroup memory.current for both gateway containers. Reported peaks are the largest same-sample sum across the two pods during traffic and drain. Each metric can peak at a different time. RSS and socket figures must not be added to total cgroup memory. Finite sampling can miss brief spikes.

| Load | Waiters per pod | Typical total MiB | Peak total MiB | Peak process RSS MiB | Peak socket MiB |
|---|---:|---:|---:|---:|---:|
| Steady 120/s | 0 | 112.7 | 322.7 | 335.4 | 29.6 |
| Steady 120/s | 16 | 122.1 | 313.0 | 328.9 | 28.6 |
| Bursts, 240/s average | 0 | 83.2 | 361.0 | 382.8 | 31.2 |
| Bursts, 240/s average | 16 | 138.4 | 511.3 | 531.6 | 42.2 |
| Steady 480/s | 0 | 271.9 | 493.5 | 509.5 | 28.5 |
| Steady 480/s | 16 | 301.3 | 476.0 | 485.4 | 42.7 |

## Idle forwarding cost

Twenty-four interleaved direct/gateway pairs per size per configuration. One upload at a time, reused HTTP/1.1 connections, warm services, either zero or sixteen configured waiters and zero programmed upstream delays. Both paths reached the same mock service, which hashed the complete request and confirmed the service credential. The paired difference includes the extra hop, authentication, policy, admission and forwarding.

| Waiters | Body | Direct median ms | Gateway median ms | Paired median added ms | Paired p95 added ms |
|---:|---|---:|---:|---:|---:|
| 0 | 8,192 bytes | 1.71 | 8.33 | 6.56 | 8.12 |
| 0 | 5,242,880 bytes | 5.88 | 22.44 | 16.36 | 19.38 |
| 0 | 104,857,600 bytes | 84.77 | 233.73 | 146.74 | 159.24 |
| 16 | 8,192 bytes | 1.65 | 8.35 | 6.70 | 9.45 |
| 16 | 5,242,880 bytes | 6.74 | 21.81 | 15.32 | 17.27 |
| 16 | 104,857,600 bytes | 94.32 | 235.03 | 141.05 | 149.04 |

## Workload and scope

Two freshly started, ready gateway pods (2 CPUs and 1 GiB per pod), eight upload slots and a 512 MiB serialized-body budget per pod. Waiting used the ten-second deadline. Four independent load-generator pods sent a fixed open-loop schedule directly to pod IPs, distributing traffic equally. Both configurations received the same sequence, byte lengths and digests for each seed. The higher-load trials used two seeds and reversed the order of the configurations. PostgreSQL, Redis and three mock upstream replicas were shared across runs. Each generator warmed its connections, and the first generator sent every size to both gateways before measurement.

The mix by count was 50% 8 KiB, 25% 128 KiB, 14% 512 KiB, 5% 5 MiB, 2.5% 10 MiB, 1.5% 20 MiB, 1% 40 MiB, 0.5% 80 MiB and 0.5% 100 MiB. Body shapes rotate by size across chat history, tool calls and retrieval content. All uploads went through the authenticated registered-service JSON route. Every successful reply was checked for upstream byte length, SHA-256 and the expected service credential.

Steady traffic used evenly spaced arrivals. The pulsed profile sent all 240 requests in the first 250 ms of each second (960/s during the pulse), with 750 ms until the next pulse. Clients sent complete bodies normally. The mock applied a 250 ms read delay and 500 ms response delay for the load tests. Tests bypassed ingress and TLS and used a verifying mock in place of inference. They measure the gateway with these inputs and dependencies.

The maximum client dispatch p99 across trials was 2.8 ms. The largest observed client-side socket queue was 0. Gateway containers had zero restarts or replacements. The low-load comparison has one pair of runs. Two pairs at each higher load support a directional comparison, with per-trial variation in the linked CSV.

[Build and harness provenance](provenance.json) · [Per-trial outcomes and denial causes](trials.csv) · [Aggregate results](summary.csv) · [By body class](body-classes.csv) · [Direct-path measurements](direct.json) · [Raw trial file index and checksums](checksums.sha256)
