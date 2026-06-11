#!/bin/bash
# Points the conformance rust adapter at the local a2a-rs fork (branch given
# as $1, default: an integration branch with all fixes), runs the three
# verifying cells, then restores everything.
set -euo pipefail

CONF=/Users/arkavo/Projects/intelligence/a2a-conformance
FORK=/Users/arkavo/Projects/intelligence/a2a-rs
BRANCH="${1:-conformance-verify}"

cd "$FORK"
git checkout -q "$BRANCH"

cd "$CONF"
cp adapters/rust/Cargo.toml /tmp/rust-adapter-cargo.toml.bak
# Restore the manifest AND rebuild from the restored (upstream-pinned) deps:
# a stale fork-patched binary in target/ silently poisons later --no-build runs.
trap 'cp /tmp/rust-adapter-cargo.toml.bak '"$CONF"'/adapters/rust/Cargo.toml; cd '"$CONF"'; cargo build --release --manifest-path adapters/rust/Cargo.toml >/dev/null 2>&1 || true; cd '"$FORK"'; git checkout -q main' EXIT

cat >> adapters/rust/Cargo.toml <<EOF

# TEMPORARY local-fork verification overlay (removed automatically)
[patch."https://github.com/a2aproject/a2a-rs.git"]
a2a-lf = { path = "$FORK/a2a" }
a2a-client-lf = { path = "$FORK/a2a-client" }
a2a-server-lf = { path = "$FORK/a2a-server" }
a2a-pb = { path = "$FORK/a2a-pb" }
EOF

cargo build --release --manifest-path adapters/rust/Cargo.toml

./runner/target/release/a2a-conformance-runner run --no-build \
    --cell client=rust-a2a,server=rust-a2a \
    --cell client=rust-a2a,server=arkavo-swift \
    --report /tmp/fork-verify-reports

echo "=== verifying scenarios ==="
python3 - <<'PYEOF'
import json
want = {
    ("errors/unsupported-operation-32004", "rust-a2a", "rust-a2a"): "pass",
    ("discovery/tenant-echo", "rust-a2a", "rust-a2a"): "pass",
    ("discovery/tenant-echo", "rust-a2a", "arkavo-swift"): "pass",
    ("edge/unknown-field-tolerance", "rust-a2a", "arkavo-swift"): "pass",
}
results = {}
for line in open("/tmp/fork-verify-reports/results.ndjson"):
    r = json.loads(line)
    results[(r["scenario"], r["client"], r["server"])] = r["status"]
failed = False
for key, expected in want.items():
    actual = results.get(key, "<missing>")
    marker = "OK " if actual == expected else "BAD"
    if actual != expected:
        failed = True
    print(f"{marker} {key[0]} [{key[1]} -> {key[2]}]: {actual} (want {expected})")
# Regression sweep: nothing else in these cells may be worse than baseline.
baseline = {}
for line in open("reports/baseline.ndjson"):
    r = json.loads(line)
    baseline[(r["scenario"], r["client"], r["server"])] = r["status"]
for key, status in results.items():
    if key in want:
        continue
    base = baseline.get(key)
    if base == "pass" and status != "pass":
        print(f"REGRESSION {key}: {base} -> {status}")
        failed = True
import sys
sys.exit(1 if failed else 0)
PYEOF
echo "=== fork verification complete ==="
