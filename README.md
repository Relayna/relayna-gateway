# Relayna Gateway v1.5.0 benchmark evidence

The tests use Gateway and harness commit `5174ae0610b42c0891b93f54cd7d78a659c0fec8`. Six native amd64 adaptive searches cover 5, 10, 20, 40, 80 and 100 MiB bodies. Every selected setting has three fresh-pod confirmations. The fixed-slot comparison uses the digest-pinned v1.4.0 release image and the same native runner for both versions. The opt-in queue study used three native arm64 bursts.

Open `gateway-1.5.0-benchmarks.html` for the interactive capacity report. The candidate and baseline latency reports each have HTML, JSON and CSV versions. The resource and latency archives contain the original trial measurements, including failed search trials. The opt-in burst archive contains request receipts and per-container memory samples. The verification JSON links the CI checks for the measured runtime.

The PostgreSQL password in fixture pod descriptors is redacted in the published copies. Request results, timings, resource samples, pod identifiers and admission settings are preserved. Embedded file checksums apply to the published copies. The exact harness source and build hashes are recorded in `gateway-1.5.0-harness-provenance.json`.

To replay the resource audit, extract the resource archive beside the audit script and run the script from the measured Gateway checkout. To replay the comparison audit, also extract the latency archive, copy the published workflows JSON alongside the script as `workflows.json`, and create a `publication/` directory. From this archive directory, verify all files with `shasum -a 256 -c gateway-1.5.0-evidence-SHA256SUMS`.

Report typography uses the included Manrope license.
