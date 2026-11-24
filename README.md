# Gateway v1.7.0 benchmark evidence

This archive contains the six native amd64 adaptive capacity searches of
runtime and harness commit `098141e8366fc071dc53a0bde062f497f39a070a`.
Each trial used two fresh Gateway pods with 2 CPUs, 1 GiB memory, and a
512 MiB serialized-body budget per pod.

- Open `gateway-1.7.0-benchmarks.html` for the interactive report.
- The JSON and CSV contain the combined trial observations.
- The audit and workflow records identify each source build and runner.
- The compressed archive retains all original reports, checkpoints, raw
  observations and failed trials.

Verify the files with `shasum -a 256 -c gateway-1.7.0-evidence-SHA256SUMS`.
