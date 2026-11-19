# Relayna and LiteLLM under the same upload schedule

PR #155 release build ec32558 and the official LiteLLM v1.110.0 arm64 image ran in turn on the same kind node. Each gateway had two fresh pods with two CPU cores each. Relayna allowed eight active uploads and 192 waiting uploads per pod, a ten-second admission deadline, and a 512 MiB serialized-body budget. LiteLLM used the image’s single-worker authenticated generic passthrough, a real PostgreSQL-backed virtual key, and the configured service bearer credential. A preflight confirmed a 401 for the anonymous caller and a verified 200 for the virtual key on a tiny control JSON body. [LiteLLM release](https://github.com/BerriAI/litellm/releases/tag/v1.110.0)

## Responses and timing

All trials offered traffic for 20 seconds with seed 146. Burst traffic sent 240 requests in the first 250 ms of each second. Steady traffic sent 480 evenly spaced requests per second. Each timed arm used the same 8 KiB warmup, body hashes, ordering, four independently paced clients, per-pod assignment, 30-second client deadline and three hashing mock replicas. The mock waited 250 ms before reading and 500 ms before responding. Elapsed includes completion or failure of the last in-flight request.

| Traffic | Gateway and memory per pod | Offered | HTTP 200 | Exact bytes verified | Equivalent JSON verified | Exact within 2 s | HTTP 200 p95 | Elapsed | Verified/s incl. drain |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Pulse | Relayna · 192 waiters · 1 GiB | 4,800 | 4,800 | 4,800 | 4,800 | 4,769 | 1.66 s | 20.67 s | 232.2 |
| Pulse | LiteLLM v1.110.0 · 1 GiB | 4,800 | 157 | 0 | 0 | 0 | 1.89 s | 19.26 s | 0.0 |
| Pulse | LiteLLM v1.110.0 · 4 GiB | 4,800 | 1,300 | 0 | 0 | 0 | 23.87 s | 49.26 s | 0.0 |
| Steady | Relayna · 192 waiters · 1 GiB | 9,600 | 5,899 | 5,899 | 5,899 | 2,061 | 2.62 s | 22.57 s | 261.3 |
| Steady | LiteLLM v1.110.0 · 1 GiB | 9,600 | 146 | 0 | 0 | 0 | 1.88 s | 20.00 s | 0.0 |

Latency starts at scheduled arrival. HTTP 200 p95 includes responses whose upstream JSON changed. Verified p95 is undefined for an arm with zero verified responses. The 4 GiB arm is a separate sensitivity test.

## Memory and failures

| Traffic | Gateway and memory per pod | Peak pod cgroup | Largest process RSS | Peak pod socket | 503s | Transport failures | Pods OOM-killed |
|---|---|---:|---:|---:|---:|---:|---:|
| Pulse | Relayna · 192 waiters · 1 GiB | 331.5 MiB | 297.8 MiB | 38.2 MiB | 0 | 0 | 0 |
| Pulse | LiteLLM v1.110.0 · 1 GiB | 1029.3 MiB | 1048.1 MiB | 12.1 MiB | 0 | 4,643 | 2 |
| Pulse | LiteLLM v1.110.0 · 4 GiB | 3191.6 MiB | 3118.7 MiB | 29.8 MiB | 0 | 3,483 | 0 |
| Steady | Relayna · 192 waiters · 1 GiB | 379.1 MiB | 360.8 MiB | 38.9 MiB | 3,680 | 21 | 0 |
| Steady | LiteLLM v1.110.0 · 1 GiB | 1029.2 MiB | 1033.1 MiB | 9.4 MiB | 0 | 9,454 | 2 |

Peaks use the kernel high-water mark on each pod’s parent cgroup, including the pod sandbox, startup, warmup and container replacements. The memory limit is enforced on the application container, so the parent can slightly exceed the stated cap. Process RSS is the largest sampled individual process and socket memory is the kernel charge for the pod. The individual peaks can occur at different times. Direct-to-pod routing kept each request assigned to the same benchmark lane across systems, including when a worker was restarting. A production load balancer may redistribute traffic while a replica is unhealthy.

## Payload integrity

The timed upstream streamed a byte count and SHA-256 for every completed request and checked its service credential. A separate serial control after timing compared parsed JSON field-by-field, recursively normalizing object key order and preserving arrays, values and types. Each timed receipt was matched by original size, actual forwarded length and digest to a control, or verified byte-for-byte against its fixture. The control stored field names and digests only.

| Body | Shape | Control status | Exact bytes | Equivalent JSON | Changed top-level fields |
|---|---|---:|---|---|---|
| 8,192 bytes | history | 200 | False | False | metadata |
| 131,072 bytes | tools | 200 | False | False | metadata |
| 524,288 bytes | retrieval | 200 | False | False | metadata |
| 5,242,880 bytes | history | 200 | False | False | metadata |
| 10,485,760 bytes | tools | 200 | False | False | metadata |
| 20,971,520 bytes | retrieval | 200 | False | False | metadata |
| 41,943,040 bytes | history | 200 | False | False | metadata |
| 83,886,080 bytes | tools | 500 | — | — | — |
| 104,857,600 bytes | retrieval | 200 | False | False | metadata |

Every timed LiteLLM HTTP 200 matched an independent control that differed in the top-level metadata field. An 8 KiB follow-up confirmed that the metadata field was absent at the upstream. The 80 MiB semantic probe returned HTTP 500 on two attempts, so it yielded no field comparison. All timed HTTP 200 receipts came from the 8 KiB through 20 MiB fixtures. At 4 GiB, the burst recorded 3,483 client deadlines and 17 HTTP 500s.

Both 1 GiB LiteLLM pods were OOM-killed on the 40 MiB step of the separate serial ascending-size warmup. The previous step, 20 MiB, returned 200 on both pods. Warmup outcomes are excluded from every timed denominator.

[Matched trial data](comparison.csv) · [Counts by size](by-size.csv) · [Independent semantic proofs](semantic-controls.json) · [Field presence check and 80 MiB retry](semantic-followup.json) · [Checksums](checksums.sha256) · [Relayna waiter trials](../report.md)
