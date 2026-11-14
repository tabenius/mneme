#!/usr/bin/env python3
"""Deterministic synthetic corpus for retrieval regression checks.

This is a pipeline and regression instrument, not a claim about retrieval
quality on real conversations. It exists so CI can exercise every search mode
against known judgments without committing anyone's session history.

Relevance is constructed, not judged:

- grade 2: document is in the query's topic and entity
- grade 0: everything else
- unsupported queries have no positives by construction
- one known-item phrase query per topic has exactly its exact matches as positives

Paragraph text uses topic vocabulary plus deliberate distractors, so an
exact-term match alone is not sufficient for a good score.
"""

import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCHEMA_SOURCE = ROOT / "src/store.rs"

# Topics that map onto curated concepts in knowledge/base.json, so concept
# expansion has real work to do; and topics that do not, so lexical-only
# retrieval is visibly insufficient.
TOPICS = {
    "repository": {
        "concept": "repository",
        "terms": ["repository", "branch", "remote", "clone"],
        "queries": ["repo", "repositories", "clone remote"],
    },
    "deployment": {
        "concept": "deployment",
        "terms": ["deployment", "systemd", "service", "restart"],
        "queries": ["deploy", "installed", "service restart"],
    },
    "directory": {
        "concept": "directory",
        "terms": ["directory", "path", "folder", "layout"],
        "queries": ["folder", "paths", "layout"],
    },
    "evidence": {
        "concept": "evidence",
        "terms": ["evidence", "provenance", "claim", "quote"],
        "queries": ["provenance", "evidence", "claims"],
    },
    "publishing": {
        "concept": "publishing",
        "terms": ["publishing", "release", "artifact", "checksum"],
        "queries": ["publish", "artifact", "release"],
    },
    "observability": {
        "concept": "observability",
        "terms": ["logging", "metrics", "trace", "alert"],
        "queries": ["observability", "logging", "telemetry"],
    },
    "network": {
        "concept": "network",
        "terms": ["network", "mesh", "ssh", "route"],
        "queries": ["mesh", "network", "ssh"],
    },
    "worktree": {
        "concept": None,
        "terms": ["worktree", "checkout", "merge", "rebase"],
        "queries": ["parallel checkouts", "side checkout", "worktree"],
    },
    "schema": {
        "concept": None,
        "terms": ["sqlite", "schema", "migration", "checkpoint"],
        "queries": ["database layout", "schema change", "migration"],
    },
    "retrieval": {
        "concept": None,
        "terms": ["ranking", "recall", "cosine", "candidate"],
        "queries": ["relevance order", "search quality", "scoring"],
    },
    "packaging": {
        "concept": None,
        "terms": ["deb", "tarball", "musl", "package"],
        "queries": ["distribution bundle", "installer", "deb"],
    },
    "concurrency": {
        "concept": None,
        "terms": ["semaphore", "deadline", "lock", "worker"],
        "queries": ["parallel limits", "timeout", "worker slots"],
    },
}

ENTITIES = ["mneme", "nostoi", "frog", "weftmark"]

# Deliberately unrelated to every topic: these must abstain rather than return
# a confident hit.
UNSUPPORTED = [
    "kubernetes helm chart rollback procedure",
    "quantum annealing benchmark",
    "sourdough hydration schedule",
    "bicycle derailleur cable tension",
]

ROLES = ["user", "assistant", "user"]

TEMPLATES = [
    "{Entity} {topic}: the {t0} touched the {t1} while {t2} was pending.",
    "Reran the {t0} after {t1}; {t2} stayed unchanged and {t3} was not verified.",
    "We changed the {t0} so {t1} remains stable across {t2} and {t3}.",
    "Checking {t0}/{t1} in this {entity} session; {t2} is still a guess.",
]


def schema_sql():
    """Read the authoritative schema instead of duplicating it here."""
    source = SCHEMA_SOURCE.read_text()
    match = re.search(r'const SCHEMA: &str = r#"(.*?)"#;', source, re.S)
    if not match:
        raise SystemExit("Could not locate SCHEMA in src/store.rs")
    return match.group(1)


def digest(*parts):
    return hashlib.sha256(json.dumps(parts, sort_keys=True).encode()).hexdigest()


def build(seed=20261114):
    """Return (documents, queries, sessions) deterministically."""
    docs = []
    sessions = []
    topics = list(TOPICS)
    sequence = 0
    for topic_index, topic in enumerate(topics):
        config = TOPICS[topic]
        terms = config["terms"]
        for entity_index, entity in enumerate(ENTITIES):
            session_id = f"ses_{topic}_{entity}"
            created = 1_700_000_000_000 + sequence * 86_400_000
            sequence += 1
            session_key = digest("synthetic", "session", session_id)
            sessions.append(
                {
                    "key": session_key,
                    "id": session_id,
                    "title": f"{topic} work for {entity}",
                    "directory": f"/synthetic/{entity}",
                    "project": f"project:{entity}",
                    "created": created,
                    "updated": created + 3000,
                }
            )
            for n in range(3):
                doc_id = f"d{len(docs):04d}"
                template = TEMPLATES[(topic_index + entity_index + n) % len(TEMPLATES)]
                text = template.format(
                    Entity=entity.capitalize(),
                    entity=entity,
                    topic=topic,
                    t0=terms[(n + topic_index) % len(terms)],
                    t1=terms[(n + topic_index + 1) % len(terms)],
                    t2=terms[(n + topic_index + 2) % len(terms)],
                    t3=terms[(n + topic_index + 3) % len(terms)],
                )
                # A neutral distractor sentence keeps documents from being pure
                # topic keywords and makes precision mean something.
                other = topics[(topic_index * 5 + n + 3) % len(topics)]
                text += (
                    f" Unrelated note: the {TOPICS[other]['terms'][0]} "
                    f"check was deferred."
                )
                docs.append(
                    {
                        "doc_id": doc_id,
                        "key": digest("synthetic", "message", session_id, n),
                        "session_id": session_id,
                        "session_key": session_key,
                        "role": ROLES[n],
                        "created": created + n * 1000,
                        "topic": topic,
                        "entity": entity,
                        "text": text,
                    }
                )

    queries = []
    for topic_index, topic in enumerate(topics):
        config = TOPICS[topic]
        for variant, phrase in enumerate(config["queries"]):
            entity = ENTITIES[(topic_index + variant) % len(ENTITIES)]
            queries.append(
                {
                    "id": f"q_{topic}_{variant}",
                    "family": topic,
                    "query": f"{phrase} in {entity} sessions",
                    "relevant": {
                        doc["doc_id"]: 2
                        for doc in docs
                        if doc["topic"] == topic and doc["entity"] == entity
                    },
                }
            )
        # One known-item phrase per topic keeps literal and strict-AND FTS
        # honest: exact evidence must still be findable, at P@1=1.
        terms = config["terms"]
        phrase = (
            f"the {terms[topic_index % len(terms)]} "
            f"touched the {terms[(topic_index + 1) % len(terms)]}"
        )
        relevant = {
            doc["doc_id"]: 2 for doc in docs if phrase.lower() in doc["text"].lower()
        }
        assert relevant, f"exact phrase not present: {phrase}"
        queries.append(
            {
                "id": f"q_{topic}_exact",
                "family": topic,
                "query": phrase,
                "relevant": relevant,
            }
        )
    for index, phrase in enumerate(UNSUPPORTED):
        queries.append(
            {
                "id": f"q_unsupported_{index}",
                "family": "unsupported",
                "query": phrase,
                "relevant": {},
            }
        )
    return docs, queries, sessions


def corpus_hash(docs, queries):
    return digest(
        [
            [doc["doc_id"], doc["key"], doc["text"]]
            for doc in docs
        ],
        [[query["id"], query["query"], query["relevant"]] for query in queries],
    )


def write_index(path, docs, sessions, connection="connected"):
    """Create a schema-accurate, read-only-compatible index without OpenCode."""
    import sqlite3

    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        path.unlink()
    database = sqlite3.connect(path)
    try:
        database.executescript(schema_sql())
        database.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('generation',1)")
        database.execute(
            "INSERT INTO sources(id,harness,version,reconciled,error,connection)"
            " VALUES(?,?,?,?,?,?)",
            ("synthetic", "opencode", "synthetic.1", 1_700_000_000_000, None, connection),
        )
        for session in sessions:
            database.execute(
                "INSERT INTO sessions(key,source,id,parent,project,directory,title,"
                "agent,model,created,updated,indexed,cost,tokens_input,"
                "tokens_output,deleted,raw) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    session["key"],
                    "synthetic",
                    session["id"],
                    None,
                    session["project"],
                    session["directory"],
                    session["title"],
                    "build",
                    "synthetic-model",
                    session["created"],
                    session["updated"],
                    session["updated"],
                    0.0,
                    0,
                    0,
                    0,
                    json.dumps({"id": session["id"]}),
                ),
            )
        for doc in docs:
            database.execute(
                "INSERT INTO messages(key,session_key,id,role,created,text,raw)"
                " VALUES(?,?,?,?,?,?,?)",
                (
                    doc["key"],
                    doc["session_key"],
                    doc["doc_id"],
                    doc["role"],
                    doc["created"],
                    doc["text"],
                    json.dumps({"id": doc["doc_id"], "type": doc["role"]}),
                ),
            )
        database.commit()
    finally:
        database.close()


def main():
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, help="Directory for index.sqlite")
    parser.add_argument("--dump", action="store_true", help="Also write corpus.json")
    args = parser.parse_args()
    docs, queries, sessions = build()
    output = Path(args.output)
    write_index(output / "index.sqlite", docs, sessions)
    if args.dump:
        (output / "corpus.json").write_text(
            json.dumps({"documents": docs, "queries": queries}, indent=2)
        )
    print(
        json.dumps(
            {
                "documents": len(docs),
                "sessions": len(sessions),
                "queries": len(queries),
                "families": len({query["family"] for query in queries}),
                "corpus_sha256": corpus_hash(docs, queries),
                "index": str((output / "index.sqlite").resolve()),
            }
        )
    )


if __name__ == "__main__":
    main()
