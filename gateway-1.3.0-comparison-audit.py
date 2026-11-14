"""Reanalyze the same-runner 1.3.0 / immutable 1.2.0 fixed-slot comparison."""
import base64
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parent
REPO = Path.cwd()
sys.path.insert(0, str(REPO / "benches/body-admission"))
import capacity_report as report
import capacity_sweep as sweep

PUB = ROOT / "publication"
CANDIDATE = "2985a614aa3afda44f32975d90b34f40acb583ce"
BASELINE = "b0bad48ca38d26a9709403a32dbff00531bb32fb"
INDEX = "sha256:a22c3d850ab2c7320d4bf87ce83adf0fa3deb90496fba6a8227a6eea9d885cbf"
manifest = json.loads((ROOT / "baseline-amd64-manifest.json").read_text())
variants = {}
body_hashes, script_hashes, hosts = set(), set(), []
for name, commit in (("candidate", CANDIDATE), ("baseline", BASELINE)):
    folder = ROOT / "comparison" / name
    original = json.loads((folder / "report.json").read_text())
    run, size = original["run"], original["sizes"][0]
    assert original["status"] == run["status"] == "completed"
    assert run["native_verified"] and run["architecture"] == "amd64"
    assert not run["harness_dirty"] and run["harness_commit"] == CANDIDATE
    assert run["image"]["source_commit"] == commit
    assert run["resources"] == dict(pods=2, cpu_per_pod=2, memory_mib_per_pod=1024, body_budget_mib_per_pod=512)
    assert size["slots"] == 8 and size["cohort_per_pod"] == 16 and len(size["trials"]) == 3
    assert size["complete"] and size["http_clean_trials"] == 3 and not size["error"]
    assert all(node["architecture"] == "amd64" for node in run["nodes"])
    if name == "baseline":
        assert run["image"]["reference"] == "ghcr.io/sarattha/relayna-gateway@" + INDEX
        assert run["image"]["id"] == manifest["config"]["digest"]
        assert run["image"]["version"] == "1.2.0"
    for filename, digest in run["harness_sha256"].items():
        source = subprocess.check_output(["git", "show", f"{CANDIDATE}:benches/body-admission/{filename}"])
        assert hashlib.sha256(source).hexdigest() == digest
    hosts.append(run["preflight"])
    seen_pods = set()
    rows = size["trials"]
    for row in rows:
        trial = folder / "5mib" / row["directory"]
        actual = sweep.analyze(trial, 8, row["attempt"], 5 * 1024**2, 16)
        normalized = report.normalize(actual)
        for key in set(actual) & set(row):
            assert normalized.get(key, actual[key]) == row[key], (name, row["directory"], key)
        assert actual["clean"] and actual["terminal_zero"]
        assert not any(actual[k] for k in ("oom_pods", "restarts", "transport_errors", "incomplete", "memory_censored"))
        assert not seen_pods & set(actual["pod_names"])
        seen_pods.update(actual["pod_names"])
        body_hashes.add(actual["body_sha256"])
        env = json.loads((trial / "environment.json").read_text())
        assert env["source_commit"] == commit and env["image_id"] == run["image"]["id"]
        assert env["read_delay_ms"] == 10000 and env["delay_ms"] == 2000
        assert env["admission_environment"] == {} and env["image_architecture"] == "amd64"
        script_hashes.add(json.dumps(env["scripts_sha256"], sort_keys=True))
    stats = {"run":run,"trials":rows,"success_latency_ms":{p:max(row["success_latency_ms"][p] for row in rows) for p in ("p50","p95","p99")},
             "rejection_p95_ms":max(row["rejection_latency_ms"]["p95"] for row in rows),
             "elapsed_ms":max(row["elapsed_ms"] for row in rows),
             "rss_peak_mib":size["rss_peak_mib"],"cgroup_peak_mib":size["cgroup_peak_mib"],
             "verified_successes":sum(row["successes"] for row in rows),
             "http_503":sum(row["statuses"].get("503",0) for row in rows),
             "transport_errors":0,"ooms":0,"restarts":0,"clean_trials":3}
    variants[name] = stats
    prefix = f"gateway-1.3.0-{name}-latency"
    (PUB / (prefix+".json")).write_bytes((folder/"report.json").read_bytes())
    (PUB / (prefix+".csv")).write_bytes((folder/"trials.csv").read_bytes())
    page = (folder/"report.html").read_text()
    font=base64.b64encode((REPO/"docs/assets/brand/fonts/Manrope-4.505.woff2").read_bytes()).decode()
    page=page.replace("<style>","<style>@font-face{font-family:Manrope;src:url(data:font/woff2;base64,"+font+") format('woff2');font-weight:200 800}body{--font-sans:Manrope,system-ui}",1)
    page=page.replace("Gateway body benchmark", "Gateway "+("1.3.0 candidate" if name=="candidate" else "1.2.0 baseline")+" · same-runner latency")
    page=page.replace('href="report.json"',f'href="{prefix}.json"').replace('href="trials.csv"',f'href="{prefix}.csv"')
    page=page.replace('href="run.json"','href="gateway-1.3.0-comparison-audit.json"').replace('Run checkpoint</a>','Comparison audit</a>')
    page=page.replace('href="provision.log"','href="gateway-1.3.0-latency-evidence.tar.gz"').replace('Setup log</a>','Original raw evidence</a>')
    (PUB/(prefix+".html")).write_text(page)
assert hosts[0] == hosts[1] and len(body_hashes) == len(script_hashes) == 1
audit={"status":"complete","workflow":"https://github.com/sarattha/relayna-gateway/actions/runs/43596307023",
       "method":"Same native runner; sequential candidate then baseline; 5 MiB, eight slots/pod, 16 offered/pod, three fresh-pod trials each; original measurements unchanged. Every trial reanalyzed and HTTP-clean.",
       "limitation":"Round-trip latency includes 12 seconds of deliberate upstream delay. Three trials do not isolate Gateway CPU overhead or establish a production latency SLO or statistical regression bound.",
       "variants":variants}
(PUB/"gateway-1.3.0-comparison-audit.json").write_text(json.dumps(audit,indent=2)+"\n")
paths=sorted(p for p in (ROOT/"comparison").rglob("*") if p.is_file())
(ROOT/"comparison-SHA256SUMS").write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(ROOT)}\n' for p in paths))
with tarfile.open(PUB/"gateway-1.3.0-latency-evidence.tar.gz","w:gz") as tar:
    tar.add(ROOT/"comparison",arcname="comparison")
    for name in ("comparison-SHA256SUMS","baseline-manifest.json","baseline-amd64-manifest.json","comparison-gitleaks.log"):
        tar.add(ROOT/name,arcname=name)
(PUB/"gateway-1.3.0-comparison-audit.py").write_bytes(Path(__file__).read_bytes())
print(json.dumps({name:{k:v for k,v in stats.items() if k not in ("run","trials")} for name,stats in variants.items()},indent=2))
