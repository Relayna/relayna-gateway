# Gateway v1.7.1 benchmark evidence

The six native amd64 adaptive searches measured runtime and harness commit
`c6d572ff77e9d7eaf7db40ed3a1f92b8dfccbff5`. Each trial used two fresh
Gateway pods with 2 CPUs, 1 GiB memory and a 512 MiB serialized-body budget per pod.

The HTML report links to combined JSON and CSV, the audit, and the original
reports, raw observations, checkpoints and failed trials in the compressed archive.
Verify all files with `shasum -a 256 -c gateway-1.7.1-evidence-SHA256SUMS`.
