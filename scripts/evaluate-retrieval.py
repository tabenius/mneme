#!/usr/bin/env python3
"""Frozen, fully judged small-corpus retrieval and grouped reranker evaluation.

Inputs and per-document outputs stay private. Does not use network or LLM APIs.
Labels and predictions must exist before scoring; hashes are recorded in reports.
"""

import os

os.environ.setdefault("OPENBLAS_NUM_THREADS", "2")
os.environ.setdefault("OMP_NUM_THREADS", "2")
import argparse
import collections
import hashlib
import importlib.util
import json
import math
import subprocess
import sys
import time
from pathlib import Path

import numpy as np
import sklearn
from sklearn.dummy import DummyClassifier
from sklearn.linear_model import LogisticRegression
from sklearn.model_selection import GroupKFold
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler
from sklearn.svm import LinearSVC
from sklearn.tree import DecisionTreeClassifier

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "mneme_retrieval", ROOT / "scripts/retrieval.py"
)
engine = importlib.util.module_from_spec(spec)
spec.loader.exec_module(engine)


def metrics(order, gold, universe, k=5):
    selected = list(dict.fromkeys(order))[:k]
    positives = {d for d, g in gold.items() if g > 0 and d in universe}
    predicted = set(selected)
    tp = len(predicted & positives)
    fp = len(predicted - positives)
    fn = len(positives - predicted)
    tn = len(universe - predicted - positives)
    gain = lambda d: 2 ** gold.get(d, 0) - 1
    dcg = sum(gain(d) / math.log2(i + 2) for i, d in enumerate(selected))
    ideal = sorted((gold[d] for d in positives), reverse=True)[:k]
    idcg = sum((2**g - 1) / math.log2(i + 2) for i, g in enumerate(ideal))
    return {
        "tp": tp,
        "fp": fp,
        "fn": fn,
        "tn": tn,
        "precision": tp / (tp + fp) if tp + fp else None,
        "precision_at_5": tp / k,
        "recall_at_5": tp / len(positives) if positives else None,
        "sensitivity": tp / (tp + fn) if tp + fn else None,
        "specificity": tn / (tn + fp) if tn + fp else None,
        "fdr": fp / (tp + fp) if tp + fp else None,
        "ndcg_at_5": dcg / idcg if idcg else None,
        "mrr": next((1 / (i + 1) for i, d in enumerate(selected) if d in positives), 0)
        if positives
        else None,
        "returned": len(selected),
        "answerable": bool(positives),
    }


def aggregate(rows):
    result = {}
    for field in [
        "precision",
        "precision_at_5",
        "recall_at_5",
        "sensitivity",
        "specificity",
        "fdr",
        "ndcg_at_5",
        "mrr",
        "returned",
    ]:
        values = [r[field] for r in rows if r[field] is not None]
        result[field] = round(float(np.mean(values)), 6) if values else None
    result["defined_queries"] = {
        f: sum(r[f] is not None for r in rows)
        for f in ["precision", "recall_at_5", "fdr"]
    }
    result["confusion_totals"] = {
        f: sum(r[f] for r in rows) for f in ["tp", "fp", "fn", "tn"]
    }
    return result


def interval_delta(rows_a, rows_b, field, seed=4242):
    # Pair and resample intent families, not near-duplicate paraphrases independently.
    groups = collections.defaultdict(list)
    for a, b in zip(rows_a, rows_b):
        assert (a.get("query_id"), a.get("fold")) == (b.get("query_id"), b.get("fold"))
        if a[field] is not None and b[field] is not None:
            groups[a["family"]].append(a[field] - b[field])
    if not groups:
        return None
    means = np.array([np.mean(v) for v in groups.values()])
    rng = np.random.default_rng(seed)
    samples = np.mean(rng.choice(means, size=(2000, len(means)), replace=True), axis=1)
    return {
        "mean_family_delta": round(float(means.mean()), 6),
        "descriptive_95_interval": [
            round(float(v), 6) for v in np.quantile(samples, [0.025, 0.975])
        ],
        "families": len(means),
    }


def models():
    return {
        "negative_dummy": DummyClassifier(strategy="constant", constant=0),
        "logistic": make_pipeline(
            StandardScaler(),
            LogisticRegression(
                C=0.1, class_weight="balanced", max_iter=2000, random_state=42
            ),
        ),
        "linear_svm": make_pipeline(
            StandardScaler(),
            LinearSVC(C=0.1, class_weight="balanced", max_iter=10000, random_state=42),
        ),
        "tree": DecisionTreeClassifier(
            max_depth=3, min_samples_leaf=10, class_weight="balanced", random_state=42
        ),
    }


def evaluate(args):
    folder = Path(args.fixture).resolve()
    manifest = json.loads((folder / "frozen.json").read_text())
    raw = (folder / "judgments.json").read_bytes()
    if hashlib.sha256(raw).hexdigest() != manifest["judgments_sha256"]:
        raise ValueError("Judgments changed after freeze")
    docs = json.loads((folder / "corpus.json").read_text())
    judgments = json.loads(raw)
    if (
        hashlib.sha256((folder / "corpus.json").read_bytes()).hexdigest()
        != manifest["corpus_sha256"]
    ):
        raise ValueError("Corpus changed after freeze")
    queries = judgments["queries"]
    k = json.loads((ROOT / "knowledge/base.json").read_text())
    if (
        hashlib.sha256((ROOT / "knowledge/base.json").read_bytes()).hexdigest()
        != manifest["knowledge_sha256"]
    ):
        raise ValueError(
            "Vocabulary changed after freeze; create a new versioned experiment, not a replacement score"
        )
    universe = {d["doc_id"] for d in docs}
    key_to_id = {d["key"]: d["doc_id"] for d in docs}
    by_id = {d["doc_id"]: d for d in docs}
    records = {
        m: []
        for m in [
            "assistant_inspection",
            "literal",
            "fts",
            "ranked_fts",
            "concept",
            "semantic",
            "hybrid",
        ]
    }
    rankings = {}
    feature_rows = []
    for q in queries:
        for mode in records:
            start = time.perf_counter()
            if mode == "assistant_inspection":
                order = q["assistant"]
            elif mode in ("literal", "fts"):
                command = [
                    args.binary,
                    "--db",
                    str(folder / "index.sqlite"),
                    "query",
                    "history_search",
                    "--args",
                    json.dumps({"query": q["query"], "mode": mode, "limit": 100}),
                ]
                value = json.loads(subprocess.check_output(command, timeout=20))
                order = [
                    key_to_id[r["evidence_ref"].split(":", 1)[1]]
                    for r in value["items"]
                ]
            else:
                value = engine.run(
                    {
                        "db": str(folder / "index.sqlite"),
                        "tool": "history_search",
                        "query": {"query": q["query"], "mode": mode, "limit": 100},
                        "knowledge": k,
                    }
                )
                order = [
                    key_to_id[r["evidence_ref"].split(":", 1)[1]]
                    for r in value["items"]
                ]
            record = metrics(order, q["relevant"], universe)
            record.update(
                {
                    "query_id": q["id"],
                    "family": q["family"],
                    "latency_ms": round((time.perf_counter() - start) * 1000, 3),
                }
            )
            records[mode].append(record)
            rankings[(q["id"], mode)] = order
        # Same production features; zero for absent lexical/vector candidates.
        p = engine.plan({"query": q["query"]}, k)
        vectors, _ = engine.vector_candidates(str(folder / "index.sqlite"), {}, p)
        order = rankings[(q["id"], "ranked_fts")]
        lex = {d: 1 / (i + 1) for i, d in enumerate(order)}
        for d in docs:
            f = engine.features(
                p,
                d,
                k,
                vectors.get(d["key"], {}).get("score", 0),
                lex.get(d["doc_id"], 0),
            )
            feature_rows.append(
                {
                    "query_id": q["id"],
                    "family": q["family"],
                    "doc_id": d["doc_id"],
                    "session_group": d["session_id"],
                    "features": f,
                    "label": int(q["relevant"].get(d["doc_id"], 0) > 0),
                }
            )
    x = np.array([r["features"] for r in feature_rows])
    y = np.array([r["label"] for r in feature_rows])
    experiments = {}
    for protocol, group_field in [
        ("unseen_sessions", "session_group"),
        ("unseen_query_families", "family"),
    ]:
        groups = np.array([r[group_field] for r in feature_rows])
        fold_rows = {
            m: []
            for m in [
                *models(),
                *[n + "_rank_only" for n in models() if n != "negative_dummy"],
                "hybrid",
            ]
        }
        classifier_counts = {m: collections.Counter() for m in models()}
        for fold, (train, test) in enumerate(
            GroupKFold(n_splits=3).split(x, y, groups)
        ):
            assert not set(groups[train]) & set(groups[test])
            for name, model in models().items():
                model.fit(x[train], y[train])
                prediction = model.predict(x[test])
                for actual, pred in zip(y[test], prediction):
                    classifier_counts[name][
                        "tp"
                        if actual and pred
                        else "fp"
                        if pred
                        else "fn"
                        if actual
                        else "tn"
                    ] += 1
                if hasattr(model, "decision_function"):
                    scores = model.decision_function(x[test])
                elif name == "negative_dummy":
                    scores = np.zeros(len(test))
                else:
                    scores = model.predict_proba(x[test])[:, 1]
                for q in queries:
                    pairs = [
                        (i, score, pred)
                        for i, score, pred in zip(test, scores, prediction)
                        if feature_rows[i]["query_id"] == q["id"]
                    ]
                    if not pairs:
                        continue
                    available = {feature_rows[i]["doc_id"] for i, _, _ in pairs}
                    candidates = set(rankings[(q["id"], "hybrid")])
                    ordered = [
                        (i, score, pred)
                        for i, score, pred in sorted(
                            pairs, key=lambda r: (-r[1], feature_rows[r[0]]["doc_id"])
                        )
                        if feature_rows[i]["doc_id"] in candidates
                    ]
                    ranked = [
                        feature_rows[i]["doc_id"] for i, score, pred in ordered if pred
                    ]
                    row = metrics(ranked, q["relevant"], available)
                    row.update(
                        {"family": q["family"], "query_id": q["id"], "fold": fold}
                    )
                    fold_rows[name].append(row)
                    if name != "negative_dummy":
                        order = [feature_rows[i]["doc_id"] for i, _, _ in ordered]
                        row = metrics(order, q["relevant"], available)
                        row.update(
                            {"family": q["family"], "query_id": q["id"], "fold": fold}
                        )
                        fold_rows[name + "_rank_only"].append(row)
            for q in queries:
                available = {
                    feature_rows[i]["doc_id"]
                    for i in test
                    if feature_rows[i]["query_id"] == q["id"]
                }
                if not available:
                    continue
                order = [d for d in rankings[(q["id"], "hybrid")] if d in available]
                row = metrics(order, q["relevant"], available)
                row.update({"family": q["family"], "query_id": q["id"], "fold": fold})
                fold_rows["hybrid"].append(row)
        classification = {}
        for name, count in classifier_counts.items():
            tp, fp, fn, tn = [count[k] for k in ["tp", "fp", "fn", "tn"]]
            sensitivity = tp / (tp + fn) if tp + fn else None
            specificity = tn / (tn + fp) if tn + fp else None
            classification[name] = {
                **{k: count[k] for k in ["tp", "fp", "fn", "tn"]},
                "sensitivity": sensitivity,
                "specificity": specificity,
                "precision": tp / (tp + fp) if tp + fp else None,
                "fdr": fp / (tp + fp) if tp + fp else None,
                "balanced_accuracy": (sensitivity + specificity) / 2
                if sensitivity is not None and specificity is not None
                else None,
            }
        experiments[protocol] = {
            "ranking": {name: aggregate(rows) for name, rows in fold_rows.items()},
            "classification": classification,
            "paired_delta_vs_hybrid": {
                name: {
                    f: interval_delta(rows, fold_rows["hybrid"], f)
                    for f in ["ndcg_at_5", "recall_at_5", "precision_at_5"]
                }
                for name, rows in fold_rows.items()
                if name != "hybrid"
            },
        }
    report = {
        "experiment_version": 2,
        "evaluation_code_sha256": hashlib.sha256(
            Path(__file__).read_bytes()
        ).hexdigest(),
        "retrieval_code_sha256": hashlib.sha256(
            (ROOT / "scripts/retrieval.py").read_bytes()
        ).hexdigest(),
        "manifest": manifest,
        "documents": len(docs),
        "queries": len(queries),
        "query_families": len(set(q["family"] for q in queries)),
        "judgment_policy": judgments["policy"],
        "assistant_baseline_caveat": judgments["assistant_baseline_caveat"],
        "methods": {name: aggregate(rows) for name, rows in records.items()},
        "paired_delta_vs_fts": {
            name: interval_delta(rows, records["fts"], "ndcg_at_5")
            for name, rows in records.items()
            if name != "fts"
        },
        "paired_delta_vs_ranked_fts": {
            name: interval_delta(rows, records["ranked_fts"], "ndcg_at_5")
            for name, rows in records.items()
            if name != "ranked_fts"
        },
        "supervised": experiments,
        "features": engine.FEATURES,
        "sklearn_version": sklearn.__version__,
        "limitations": [
            "Assistant-authored judgments are not independent human ground truth.",
            "Recall/sensitivity/specificity apply only to the fully judged frozen small corpus, not the full session archive.",
            "Unsupervised LSA indexes the evaluation documents without relevance labels; this is transductive retrieval, not supervised label leakage.",
            "Hyperparameters fixed before scores: linear SVM C=.1; logistic C=.1; tree depth=3/min leaf=10; balanced classes. No held-out tuning.",
            "Session and query-family holdouts are separate stress tests, not a simultaneous crossed holdout.",
            "Confidence intervals are descriptive family-cluster bootstraps; small sample and topic selection limit generalization.",
            "Classifier sensitivity/specificity use all held-out pairs; retrieval metrics use top five. SVM margins/tree scores are not calibrated probabilities.",
            "Rank-only rerankers preserve the hybrid candidate pool/guards; unsuffixed supervised methods additionally filter by the classifier threshold. Version 1 without this distinction is retained locally, not used to select the final conclusion.",
            "Assistant manual predictions and labels share an evaluator; do not claim an independent head-to-head model win.",
        ],
        "per_query": records,
    }
    # Secondary descriptive trade-off, not a tuned threshold or replacement for primary k=5.
    report["secondary_top_k_sweep"] = {}
    for mode in records:
        report["secondary_top_k_sweep"][mode] = {}
        for cutoff in [1, 3, 5]:
            rows = [
                metrics(rankings[(q["id"], mode)], q["relevant"], universe, k=cutoff)
                for q in queries
            ]
            summary = aggregate(rows)
            report["secondary_top_k_sweep"][mode][str(cutoff)] = {
                "precision_among_returns": summary["precision"],
                "precision_at_k": summary["precision_at_5"],
                "recall_at_k": summary["recall_at_5"],
                "fdr_among_returns": summary["fdr"],
                "ndcg_at_k": summary["ndcg_at_5"],
                "defined_queries": {
                    "precision": summary["defined_queries"]["precision"],
                    "recall_at_k": summary["defined_queries"]["recall_at_5"],
                    "fdr": summary["defined_queries"]["fdr"],
                },
            }
    (folder / "results.json").write_text(json.dumps(report, indent=2))
    disputes = []
    for q in queries:
        for mode in ["fts", "concept", "semantic", "hybrid"]:
            retrieved = rankings[(q["id"], mode)][:5]
            fp = [d for d in retrieved if not q["relevant"].get(d)]
            missed = [d for d in q["relevant"] if d not in retrieved]
            if fp or missed:
                disputes.append(
                    {
                        "query_id": q["id"],
                        "family": q["family"],
                        "query": q["query"],
                        "method": mode,
                        "false_positive_docs": fp,
                        "missed_docs": missed,
                    }
                )
    (folder / "review-disagreements.json").write_text(json.dumps(disputes, indent=2))
    print(
        json.dumps(
            {
                **{key: report[key] for key in ["documents", "queries", "methods"]},
                "report_path": str(folder / "results.json"),
            }
        )
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", required=True)
    parser.add_argument("--binary", default="target/debug/mneme")
    evaluate(parser.parse_args())
