#!/usr/bin/env python3
"""End-to-end CLI timings including process startup; never prints query results."""
import argparse
import json
import statistics
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument("--binary", default="target/release/mneme")
parser.add_argument("--db", required=True)
parser.add_argument("--repeats", type=int, default=20)
args = parser.parse_args()
assert 1 <= args.repeats <= 1000
results = {}
for label, tool, filters in [
    ("latest_five", "history_list_sessions", {"limit": 5}),
    ("fts", "history_search", {"query": "session", "mode": "fts", "limit": 20}),
    ("literal", "history_search", {"query": "worktree", "limit": 20}),
    ("tool_failures", "history_get_tool_runs", {"status": "error", "limit": 20}),
    ("aggregate", "history_aggregate", {"group_by": "role"}),
]:
    samples = []
    for _ in range(args.repeats):
        start = time.perf_counter()
        completed = subprocess.run([args.binary, "--db", args.db, "query", tool,
                                    "--args", json.dumps(filters)], check=True,
                                   stdout=subprocess.PIPE, timeout=10)
        json.loads(completed.stdout)
        samples.append((time.perf_counter() - start) * 1000)
    ordered = sorted(samples)
    results[label] = {"median_ms": round(statistics.median(samples), 2),
                      "p95_ms": round(ordered[min(len(ordered)-1, int(len(ordered)*.95))], 2)}
print(json.dumps({"method": "warm filesystem; includes CLI process startup",
                  "repeats": args.repeats, "queries": results}))
