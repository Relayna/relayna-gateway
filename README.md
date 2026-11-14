# Gateway 1.4.0 immutable release evidence

Measured runtime: `a45b814cf7f20fe3ff33eaeff28aa77816d92f41` (Cargo 1.4.0), native amd64 on 14 November 2026 UTC.
All six adaptive studies completed,263 resource trials reanalyzed,and three fresh-pod stable confirmations selected per size. All are conservatively labeled tested lower bounds. Resource failures and lost denial responses remain in original evidence. A separate same-runner comparison has three HTTP-clean trials per version against the immutable latest published v1.2.0 image.

The evidence lives outside the application source tree. The documentation workflow checks the SHA-256 manifest before copying these files into its site. GitHub release attachments remain CHANGELOG.md and SPDX SBOM only.

- `gateway-1.4.0-benchmarks.html/json` and trials CSV: combined audited resource report.
- `gateway-1.4.0-audit.py/json`: raw-trial reanalysis and per-size source/image/hardware provenance. Extract resource evidence beside the script and run it from the application checkout containing the measured commit.
- `gateway-1.4.0-resource-evidence.tar.gz`: all original raw and failed trials,checkpoints,reports and original-file checksums.
- `gateway-1.4.0-candidate-latency.*`,baseline-latency.*,comparison-audit.py/json and latency-evidence.tar.gz: matched-runner comparison,original raw trials,OCI manifests and audit.
- `gateway-1.4.0-harness-source.tar.gz`: committed harness,Dockerfile,Cargo/build metadata and workflow source.
- `gateway-1.4.0-verification-evidence.tar.gz` and verification.json: local validation plus the retained initial CI failure and successful same-candidate retry context.
- `gateway-1.4.0-workflows.json`: final experiment identities and conclusions.
- `gateway-1.4.0-evidence-SHA256SUMS`: every other archive file's SHA-256.

Image version labels are unknown; exact source and resolved native image IDs are retained. These are benchmark builds,not signed published release images. Reports use Manrope with its OFL license retained. No credentials,kubeconfigs or production prompts are included. Runtime/build-input changes require new measurement; later results/docs-only commits may follow unchanged production inputs.
