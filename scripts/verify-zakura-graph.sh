#!/usr/bin/env bash
#
# Prove that zkv's resolved dependency graph rides the Zakura Common stack alone.
#
# zkv consumes the librustzcash crypto and wallet crates as their Zakura Common
# forks (`zakura-orchard`, `zakura-client-backend`, ...; see the Zcash block in
# the workspace Cargo.toml), because zecd does and the two share `WalletDb`
# handle types across the `crate::engine` seam. The forks keep the upstream
# *library* names (`orchard`, `zcash_client_backend`), so a stray crates.io
# original compiles fine right up until two identical-looking types meet at a
# call site. Compiling is therefore not proof. This script reads
# `cargo metadata` and fails if:
#
#   - any crates.io original of a crate Zakura Common forks is in the graph (an
#     edge escaped the rename: something pulled `orchard` from crates.io), or
#   - any `zakura-*` package resolves to more than one version (the wallet layer
#     pins the crypto family with `=`, so a second version means a conflicting
#     requirement crept in).
#
# The forbidden list is the crates.io name of every member of
# zakura-core/common plus the wallet-layer crates zakura-core/wallet-libraries
# forks. Extend it if Zakura Common grows. Mirrors zecd's script of the same
# name (its own CI runs the daemon-side copy), rooted at this workspace's
# members rather than a single root package.
#
# Run from anywhere; the CI `zakura-graph` job runs it on every PR.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

META="${TMPDIR:-/tmp}/zkv-graph-metadata.json"
cargo metadata --format-version 1 --locked --all-features > "$META"

python3 - "$META" <<'PY'
import json
import sys
from collections import defaultdict

FORBIDDEN = {
    # zakura-core/common (the proving/protocol stack).
    "orchard", "sapling-crypto", "zcash_primitives", "zcash_keys", "zcash_proofs",
    "halo2_proofs", "halo2_gadgets", "halo2_poseidon", "halo2_legacy_pdqsort",
    "pasta_curves", "sinsemilla", "reddsa", "redjubjub", "bellman", "bls12_381",
    "jubjub", "pairing",
    # zakura-core/wallet-libraries (the wallet layer).
    "zcash_client_backend", "zcash_client_sqlite", "pczt",
}

metadata = json.load(open(sys.argv[1]))
packages = {p["id"]: p for p in metadata["packages"]}
nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}

# Every workspace member is a root: zkv, the faucet and the oracles each resolve
# their own edges, and a stray original reached through any of them is the bug.
reachable = set()
queue = list(metadata["workspace_members"])
while queue:
    pid = queue.pop()
    if pid in reachable:
        continue
    reachable.add(pid)
    queue.extend(d["pkg"] for d in nodes[pid]["deps"])

versions = defaultdict(set)
for pid in reachable:
    versions[packages[pid]["name"]].add(packages[pid]["version"])

problems = []
for name in sorted(FORBIDDEN & versions.keys()):
    problems.append(f"{name}: crates.io original present; an edge escaped the zakura-* rename")
for name, found in sorted(versions.items()):
    if name.startswith("zakura-") and len(found) > 1:
        problems.append(f"{name}: {len(found)} versions in the graph: {sorted(found)}")
zakura = sorted(n for n in versions if n.startswith("zakura-"))
if not zakura:
    problems.append("no zakura-* package in the graph at all")

if problems:
    print("the resolved graph is not Zakura-only:", file=sys.stderr)
    for problem in problems:
        print(f"  {problem}", file=sys.stderr)
    raise SystemExit(1)

print(f"verified: {len(versions)} packages in the graph, {len(zakura)} of them zakura, no crates.io originals")
print("  " + " ".join(f"{n} {next(iter(versions[n]))}" for n in zakura))
PY
