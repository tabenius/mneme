#!/usr/bin/env python3
"""Synthetic retrieval regression gate.

Runs every search mode against a deterministic synthetic corpus and fails when
a mode drops below its floor or regresses against the committed baseline.

This is a smoke benchmark, not a quality claim: judgments are constructed from
topic and entity membership, not human relevance labels. Real quality
evaluation stays in docs/RETRIEVAL-EVALUATION.md with the private fixture.

Usage:
  python scripts/check-retrieval.py --binary target/debug/mneme
  python scripts/check-retrieval.py --binary target/debug/mneme --update-baseline
"""

import argparse
import importlib.util
import json
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASELINE = ROOT / "evaluation/synthetic-baseline.json"
MODES = ["literal", "fts", "ranked_fts", "concept", "semantic", "hybrid"]
CUTOFFS = [1, 5, 10]

# Absolute floors keep a broken pipeline from passing just because the baseline
# was regenerated after the break. Regression tolerance covers library drift.
FLOORS = {
    "ranked_fts": {"ndcg_at_k": 0.45, "recall_at_k": 0.45},
    "concept": {"ndcg_at_k": 0.65, "recall_at_k": 0.65},
    "hybrid": {"ndcg_at_k": 0.55, "recall_at_k": 0.55},
    "semantic": {"ndcg_at_k": 0.40, "recall_at_k": 0.40},
}
REGRESSION_TOLERANCE = 0.05


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


corpus = load("synthetic_corpus", ROOT / "scripts/synthetic-corpus.py")
evaluate = load("mneme_evaluate", ROOT / "scripts/evaluate-retrieval.py")
engine = load("mneme_retrieval", ROOT / "scripts/retrieval.py")


def fixture(workdir, binary, dimensions):
    docs, queries, sessions = corpus.build()
    database = workdir / "index.sqlite"
    corpus.write_index(database, docs, sessions)
    started = time.perf_counter()
    process = subprocess.run(
        [binary, "--db", str(database), "semantic-build", "--dimensions", str(dimensions)],
        capture_output=True,
        text=True,
        timeout=300,
    )
    if process.returncode != 0:
        raise SystemExit(
            "semantic-build failed; install evaluation/requirements.txt.\n"
            + (process.stderr or process.stdout)
        )
    return docs, queries, database, (time.perf_counter() - started) * 1000


def order_for(mode, text, database, binary, key_to_id, knowledge):
    request = {"query": text, "mode": mode, "limit": 100}
    if mode in ("literal", "fts"):
        output = subprocess.check_output(
            [
                binary,
                "--db",
                str(database),
                "query",
                "history_search",
                "--args",
                json.dumps(request),
            ],
            timeout=30,
        )
        value = json.loads(output)
    else:
        value = engine.run(
            {
                "db": str(database),
                "tool": "history_search",
                "query": request,
                "knowledge": knowledge,
            }
        )
    return [
        key_to_id[item["evidence_ref"].split(":", 1)[1]] for item in value["items"]
    ]


def metric_set(order, gold, universe, cutoff):
    row = evaluate.metrics(order, gold, universe, k=cutoff)
    return {
        "precision": row["precision"],
        "precision_at_k": row["precision_at_5"],
        "recall_at_k": row["recall_at_5"],
        "specificity": row["specificity"],
        "fdr": row["fdr"],
        "ndcg_at_k": row["ndcg_at_5"],
        "mrr": row["mrr"],
        "returned": row["returned"],
    }


def summarize(rows):
    fields = [
        "precision",
        "precision_at_k",
        "recall_at_k",
        "specificity",
        "fdr",
        "ndcg_at_k",
        "mrr",
    ]
    summary = {}
    for field in fields:
        values = [row[field] for row in rows if row[field] is not None]
        summary[field] = round(statistics.fmean(values), 6) if values else None
    summary["defined_queries"] = sum(row["recall_at_k"] is not None for row in rows)
    summary["abstained"] = sum(row["returned"] == 0 for row in rows)
    return summary


def run(binary, workdir, dimensions):
    docs, queries, database, build_ms = fixture(workdir, binary, dimensions)
    knowledge = json.loads((ROOT / "knowledge/base.json").read_text())
    universe = {doc["doc_id"] for doc in docs}
    key_to_id = {doc["key"]: doc["doc_id"] for doc in docs}
    results = {}
    latencies = {}
    errors = {}
    for mode in MODES:
        latencies[mode] = []
        per_cutoff = {cutoff: [] for cutoff in CUTOFFS}
        orders = {}
        for query in queries:
            started = time.perf_counter()
            try:
                order = order_for(
                    mode, query["query"], database, binary, key_to_id, knowledge
                )
            except Exception as error:  # noqa: BLE001 - reported, not hidden
                errors.setdefault(mode, []).append(f"{query['id']}: {error}")
                order = []
            latencies[mode].append((time.perf_counter() - started) * 1000)
            orders[query["id"]] = order
            for cutoff in CUTOFFS:
                per_cutoff[cutoff].append(
                    metric_set(order, query["relevant"], universe, cutoff)
                )
        results[mode] = {
            str(cutoff): summarize(per_cutoff[cutoff]) for cutoff in CUTOFFS
        }
        results[mode]["orders"] = orders
    return {
        "documents": len(docs),
        "sessions": len({doc["session_id"] for doc in docs}),
        "queries": len(queries),
        "families": len({query["family"] for query in queries}),
        "corpus_sha256": corpus.corpus_hash(docs, queries),
        "build_ms": round(build_ms, 3),
        "modes": results,
        "latency_ms": {
            mode: {
                "p50": round(statistics.median(values), 3),
                "p95": round(
                    sorted(values)[max(0, int(len(values) * 0.95) - 1)], 3
                ),
                "max": round(max(values), 3),
            }
            for mode, values in latencies.items()
        },
        "errors": errors,
    }


def evaluate_gate(report, baseline):
    failures = []
    for mode, floors in FLOORS.items():
        for field, floor in floors.items():
            value = report["modes"][mode]["5"][field]
            if value is None or value < floor:
                failures.append(
                    f"{mode} {field}={value} below floor {floor}"
                )
    for mode in ("concept", "hybrid"):
        unsupported = report["modes"][mode]["5"]["abstained"]
        # 4 of 40 queries are unsupported by construction.
        if unsupported < 4:
            failures.append(
                f"{mode} returned results for unsupported queries "
                f"({report['modes'][mode]['5']['abstained']} abstained, expected >= 4)"
            )
    if baseline:
        for mode, reference in baseline.get("modes", {}).items():
            for cutoff, metrics in reference.items():
                for field, expected in metrics.items():
                    actual = report["modes"][mode][cutoff][field]
                    if expected is None or actual is None:
                        continue
                    if actual + REGRESSION_TOLERANCE < expected:
                        failures.append(
                            f"{mode} {field}@{cutoff}={actual} regressed below "
                            f"baseline {expected}"
                        )
    for mode, messages in report["errors"].items():
        failures.append(f"{mode} query errors: {messages[:3]}")
    return failures


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/mneme")
    parser.add_argument("--workdir", help="Keep the fixture here instead of a temp dir")
    parser.add_argument("--baseline", default=str(BASELINE))
    parser.add_argument("--update-baseline", action="store_true")
    parser.add_argument("--dimensions", type=int, default=32)
    parser.add_argument("--json", action="store_true", help="Print the full report")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    if not Path(binary).exists():
        raise SystemExit(f"Binary not found: {binary} (run cargo build first)")

    baseline = None
    if Path(args.baseline).exists():
        baseline = json.loads(Path(args.baseline).read_text())

    with tempfile.TemporaryDirectory(prefix="mneme-synthetic-") as temporary:
        workdir = Path(args.workdir) if args.workdir else Path(temporary)
        workdir.mkdir(parents=True, exist_ok=True)
        report = run(binary, workdir, args.dimensions)
    report["floors"] = FLOORS
    report["regression_tolerance"] = REGRESSION_TOLERANCE

    if (
        baseline
        and not args.update_baseline
        and baseline.get("corpus_sha256") != report["corpus_sha256"]
    ):
        raise SystemExit(
            "Synthetic corpus changed (seed or generator). Rerun with "
            "--update-baseline and review the diff; do not silently replace scores."
        )

    failures = evaluate_gate(report, baseline)
    if args.update_baseline:
        reference = {
            "schema": 1,
            "note": (
                "Synthetic smoke benchmark: constructed relevance on generated text. "
                "Not evidence of quality on real conversations."
            ),
            "dimensions": args.dimensions,
            "corpus_sha256": report["corpus_sha256"],
            "floors": FLOORS,
            "modes": {
                mode: {
                    str(cutoff): {
                        field: report["modes"][mode][str(cutoff)][field]
                        for field in (
                            "precision",
                            "precision_at_k",
                            "recall_at_k",
                            "fdr",
                            "ndcg_at_k",
                            "mrr",
                        )
                    }
                    for cutoff in CUTOFFS
                }
                for mode in MODES
            },
        }
        Path(args.baseline).write_text(json.dumps(reference, indent=2) + "\n")
        print(f"Baseline written to {args.baseline}")
        failures = evaluate_gate(report, reference)

    width = max(len(mode) for mode in MODES)
    print(f"{'mode':<{width}}  nDCG@5  recall@5  P@1    p95 ms  abstained")
    for mode in MODES:
        metrics = report["modes"][mode]["5"]
        print(
            f"{mode:<{width}}  {metrics['ndcg_at_k']:<7.3f} {metrics['recall_at_k']:<9.3f} "
            f"{report['modes'][mode]['1']['precision_at_k']:<6.3f} "
            f"{report['latency_ms'][mode]['p95']:<7.1f} {metrics['abstained']}"
        )
    if args.json:
        print(json.dumps(report, indent=2))
    if failures:
        print("\nFAIL", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print(
        f"\nOK: {report['documents']} documents, {report['queries']} queries, "
        f"corpus {report['corpus_sha256'][:12]}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
