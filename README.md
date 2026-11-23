# Relayna Gateway v1.6.0 benchmark evidence

The candidate and harness were built at commit 0bbac005847be624b260d1fd57916237d1a6232b. The six native amd64 adaptive searches ran on separate hosted runners. The fixed-slot comparison ran both images on the same runner.

The resource and comparison archives retain all original trials, checkpoints, reports, and available diagnostics, including failures and the interrupted local arm64 attempt. Disposable PostgreSQL credentials and private kubeconfig paths are redacted from retained descriptors and setup logs. No kubeconfig is included. Each archive contains checksums for the sanitized files.

From the Gateway repository root, run the included audit scripts with their inputs extracted into the script directory. The fixed-slot comparison also uses the workflow manifest from this archive, copied as `workflows.json` next to the script. The versioned Gateway documentation summarizes the results.
