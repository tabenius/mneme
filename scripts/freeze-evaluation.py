#!/usr/bin/env python3
"""Materialize a private, fully judged corpus before running any quality scoring.

Candidates: JSON records containing key/doc_id/session_id/text. Judgments: all
positive grades and explicitly zero remaining pairs. Source data never enters Git.
"""

import argparse
import hashlib
import json
import os
import sqlite3
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--db", required=True)
parser.add_argument("--fixture", required=True)
args = parser.parse_args()
os.umask(0o077)
root = Path(__file__).resolve().parents[1]
folder = Path(args.fixture).resolve()
if (folder / "frozen.json").exists():
    raise SystemExit("Already frozen; use a new directory/version for a new experiment")
candidate = json.loads((folder / "candidates.json").read_text())
labels = json.loads((folder / "judgments.json").read_text())
docs = []
seen = set()
for row in candidate:
    if row["doc_id"] in labels["excluded"] or row["text"] in seen:
        continue
    seen.add(row["text"])
    docs.append(row)
ids = {d["doc_id"] for d in docs}
assert all(
    set(q["relevant"]) <= ids and set(q["assistant"]) <= ids for q in labels["queries"]
)
source = sqlite3.connect(Path(args.db).resolve().as_uri() + "?mode=ro", uri=True)
source.row_factory = sqlite3.Row
source.execute("BEGIN")
target = sqlite3.connect(folder / "index.sqlite")
tables = [
    "meta",
    "sources",
    "sessions",
    "messages",
    "tool_runs",
    "events",
    "checkpoints",
    "snapshot_hashes",
    "message_fts",
]
for table in tables:
    target.execute(
        source.execute(
            "SELECT sql FROM sqlite_master WHERE name=?", (table,)
        ).fetchone()[0]
    )
for row in source.execute(
    "SELECT sql FROM sqlite_master WHERE type IN ('index','trigger') AND sql IS NOT NULL"
):
    target.execute(row[0])
target.execute("INSERT INTO meta VALUES('generation',1)")
for row in source.execute("SELECT * FROM sources"):
    target.execute(
        "INSERT INTO sources VALUES(" + ",".join("?" for _ in row) + ")", tuple(row)
    )
sessions = set()
for d in docs:
    row = source.execute("SELECT * FROM messages WHERE key=?", (d["key"],)).fetchone()
    if row is None or row["text"] != d["text"]:
        raise SystemExit(
            "Source changed after candidate inspection; inspect a new corpus"
        )
    if row["session_key"] not in sessions:
        sessions.add(row["session_key"])
        session = source.execute(
            "SELECT * FROM sessions WHERE key=?", (row["session_key"],)
        ).fetchone()
        target.execute(
            "INSERT INTO sessions VALUES(" + ",".join("?" for _ in session) + ")",
            tuple(session),
        )
    target.execute(
        "INSERT INTO messages VALUES(" + ",".join("?" for _ in row) + ")", tuple(row)
    )
    d["created_ms"] = d["created"]
    d["role"] = row["role"]
    d["source"] = session["source"]
    d["text_length"] = len(d["text"])
target.commit()
target.close()
source.close()
(folder / "corpus.json").write_text(json.dumps(docs, ensure_ascii=False, indent=2))
manifest = {
    "version": 1,
    "frozen_at_ms": int(time.time() * 1000),
    "documents": len(docs),
    "queries": len(labels["queries"]),
    "selection": "Deterministic seed 20261024; earliest eligible 150..1500-character user message per root session; current Mneme conversation excluded. Exact text deduplicated; credential/personal/incomplete exclusions listed in judgments.",
    "judgments_sha256": hashlib.sha256(
        (folder / "judgments.json").read_bytes()
    ).hexdigest(),
    "corpus_sha256": hashlib.sha256((folder / "corpus.json").read_bytes()).hexdigest(),
    "knowledge_sha256": hashlib.sha256(
        (root / "knowledge/base.json").read_bytes()
    ).hexdigest(),
}
(folder / "frozen.json").write_text(json.dumps(manifest, indent=2))
print(
    json.dumps(
        {k: manifest[k] for k in ["version", "documents", "queries", "frozen_at_ms"]}
    )
)
