"""Local retrieval engine. stdlib for concepts; NumPy only for optional LSA queries.

No network access, model download, pickle loading or source database writes.
The same functions are used by the frozen retrieval evaluation.
"""

import collections
import datetime as dt
import hashlib
import json
import math
import os
import re
import sqlite3
import sys
import time
from pathlib import Path
from zoneinfo import ZoneInfo

STOP = set(
    "a an and are as at be been by can could did do does for from had has have how i in is it me my of on or our please show some tell that the their them there these they this those to us was we were what when where which who why will with would you your find about all only any mig min mitt mina våra vad vilka och att en ett det som om på för till har jag visa sök".split()
)
TOKEN = re.compile(r"[\w]+(?:[-./:][\w]+)*", re.UNICODE)
FEATURES = [
    "literal",
    "token_coverage",
    "concept_coverage",
    "entity",
    "title_coverage",
    "cosine",
    "lexical_rank",
    "log_length",
]


def tokens(text):
    return [t for t in TOKEN.findall(text.lower()) if t not in STOP and len(t) > 1]


def contains(text, phrase):
    return bool(
        re.search(r"(?<!\w)" + re.escape(phrase.lower()) + r"(?!\w)", text.lower())
    )


def knowledge(request):
    k = request["knowledge"]
    for collection in ("concepts", "entities"):
        # Local explicitly authored entries may replace defaults by ID.
        k[collection] = list({r["id"]: r for r in k.get(collection, [])}.values())
    for assertion in k.get("assertions", []):
        if "value" not in assertion:
            raise ValueError(
                "Knowledge assertions require an explicit value, including false or unknown/null where intended"
            )
        if not all(
            assertion.get(f)
            for f in ("subject", "predicate", "asserted_by", "source_ref", "origin")
        ):
            raise ValueError(
                "Knowledge assertions require subject, predicate, asserted_by, source_ref and origin"
            )
        if assertion["origin"] not in (
            "explicit_user",
            "project_document",
            "assistant_proposal",
            "inference",
        ):
            raise ValueError("Unsupported assertion origin")
    return k


def interval(frame, as_of):
    zone = ZoneInfo("Europe/Stockholm")
    now = dt.datetime.fromtimestamp(as_of / 1000, zone)
    midnight = now.replace(hour=0, minute=0, second=0, microsecond=0)
    if frame in ("today", "idag"):
        start, end = midnight, now
    elif frame in ("yesterday", "igår"):
        start, end = midnight - dt.timedelta(days=1), midnight
    elif frame in ("last_week", "previous_week", "last week", "förra veckan"):
        end = midnight - dt.timedelta(days=midnight.weekday())
        start = end - dt.timedelta(days=7)
    elif frame in ("this_week", "this week"):
        start, end = midnight - dt.timedelta(days=midnight.weekday()), now
    elif frame in ("past_7_days", "past seven days", "past_30_days"):
        return [as_of - (30 if frame == "past_30_days" else 7) * 86400000, as_of]
    else:
        raise ValueError(
            "Unsupported time_frame; use today, yesterday, last_week, this_week, past_7_days or past_30_days"
        )
    return [int(start.timestamp() * 1000), int(end.timestamp() * 1000)]


def plan(q, k):
    text = (q.get("query") or "").strip()
    if len(text) > 4096:
        raise ValueError("query exceeds 4096 characters")
    frame = q.get("time_frame")
    for phrase, value in [
        ("past seven days", "past_7_days"),
        ("last week", "last_week"),
        ("förra veckan", "last_week"),
        ("yesterday", "yesterday"),
        ("igår", "yesterday"),
    ]:
        if contains(text, phrase):
            frame = frame or value
            text = re.sub(re.escape(phrase), " ", text, flags=re.I)
    terms = list(dict.fromkeys(tokens(text)))[:32]
    groups = []
    expansions = []
    matched = []
    required_mentions = []
    for concept in k["concepts"]:
        hits = [a for a in concept.get("aliases", []) if contains(text, a)]
        if hits:
            matched.append(concept["id"])
            aliases = list(dict.fromkeys(hits + concept.get("aliases", [])))[:12]
            groups.append(aliases)
            expansions.append(
                {
                    "concept": concept["id"],
                    "matched": hits,
                    "aliases": aliases,
                    "related_not_synonyms": concept.get("related", []),
                }
            )
            if concept.get("attributed"):
                required_mentions.append(concept["id"])
    entities = [
        e for e in k["entities"] if any(contains(text, a) for a in e.get("aliases", []))
    ]
    explicit_entities = []
    if q.get("entity"):
        explicit_entities = [
            e
            for e in k["entities"]
            if q["entity"].lower()
            in [
                e["id"].lower(),
                e.get("name", "").lower(),
                *[a.lower() for a in e.get("aliases", [])],
            ]
        ]
        if not explicit_entities:
            raise ValueError("Unknown entity; inspect history_get_knowledge")
        entities = list({e["id"]: e for e in entities + explicit_entities}.values())
    related = {}
    for entity in entities:
        for candidate in k["entities"]:
            forward = any(
                r.get("object") == candidate["id"]
                and r.get("predicate") in ("has_repository", "located_at", "implements")
                for r in entity.get("relations", [])
            )
            reverse = any(
                r.get("object") == entity["id"]
                and r.get("predicate")
                in (
                    ("belongs_to",)
                    if entity.get("kind") == "suite"
                    else ("located_at",)
                )
                for r in candidate.get("relations", [])
            )
            if (forward or reverse) and candidate["id"] not in {
                e["id"] for e in entities
            }:
                related[candidate["id"]] = candidate
    expanded = list(
        dict.fromkeys(
            terms
            + [a for g in groups for a in g]
            + [a for e in entities for a in e.get("aliases", [])]
            + [a for e in related.values() for a in e.get("aliases", [])]
        )
    )[:96]
    filter_ids = {e["id"] for e in explicit_entities}
    if explicit_entities:
        # Explicit entity scope follows only declared one-hop links, never inferred membership.
        for entity in explicit_entities:
            for candidate in related.values():
                if entity.get("kind") == "suite" or any(
                    r.get("object") == candidate["id"]
                    for r in entity.get("relations", [])
                ):
                    filter_ids.add(candidate["id"])
    as_of = (
        q.get("as_of_ms") if q.get("as_of_ms") is not None else int(time.time() * 1000)
    )
    bounds = interval(frame, as_of) if frame else [q.get("since_ms"), q.get("until_ms")]
    if q.get("since_ms") is not None:
        bounds[0] = max(bounds[0] or q["since_ms"], q["since_ms"])
    if q.get("until_ms") is not None:
        bounds[1] = min(bounds[1] or q["until_ms"], q["until_ms"])
    if bounds[0] is not None and bounds[1] is not None and bounds[0] >= bounds[1]:
        raise ValueError("Resolved temporal interval is empty")
    return {
        "original": q.get("query"),
        "lexical_text": text,
        "terms": terms,
        "expanded_terms": expanded,
        "expansions": expansions,
        "concepts": matched,
        "entities": entities,
        "required_mentions": required_mentions,
        "priority_level": next(
            (
                level
                for level in ["p0", "p1", "p2", "low", "high"]
                if contains(text, level)
            ),
            None,
        )
        if "priority" in matched
        else None,
        "priority_levels": [
            level
            for level in ["p0", "p1", "p2", "low", "high"]
            if contains(text, level)
        ]
        if "priority" in matched
        else [],
        "filter_entity_ids": sorted(filter_ids),
        "related_entities": list(related.values()),
        "required_aliases": {
            cid: next(c["aliases"] for c in k["concepts"] if c["id"] == cid)
            for cid in required_mentions
        },
        "time_frame": frame,
        "interval_utc_ms": bounds,
        "as_of_ms": as_of,
        "timezone": "Europe/Stockholm",
        "knowledge_version": k.get("version"),
        "knowledge_fingerprint": hashlib.sha256(
            json.dumps(k, sort_keys=True).encode()
        ).hexdigest(),
        "policy": "Related concepts are not expanded as equivalent synonyms. Relevance scores are not probabilities or truth judgments.",
    }


def connection(path):
    c = sqlite3.connect(Path(path).resolve().as_uri() + "?mode=ro", uri=True, timeout=3)
    c.row_factory = sqlite3.Row
    c.execute("PRAGMA query_only=ON")
    c.execute("BEGIN")
    c.create_function(
        "mneme_contains",
        2,
        lambda text, phrase: int(contains(text or "", phrase)),
        deterministic=True,
    )
    c.create_function(
        "mneme_pattern",
        2,
        lambda text, pattern: int(bool(re.search(pattern, text or "", re.I))),
        deterministic=True,
    )
    deadline = time.monotonic() + 8
    c.set_progress_handler(lambda: time.monotonic() > deadline, 10000)
    return c


def filters(q, p):
    clauses, values = ["s.deleted=0"], []
    for field, column in [
        ("source", "s.source"),
        ("session_id", "s.id"),
        ("message_id", "m.id"),
        ("directory", "s.directory"),
        ("project_id", "s.project"),
        ("agent", "s.agent"),
        ("model", "s.model"),
        ("role", "m.role"),
    ]:
        if q.get(field) is not None:
            clauses.append(column + "=?")
            values.append(q[field])
    if q.get("include_children") is False:
        clauses.append("s.parent IS NULL")
    if q.get("entity"):
        terms = list(
            dict.fromkeys(
                a
                for e in p["entities"] + p["related_entities"]
                if e["id"] in p["filter_entity_ids"]
                for a in e.get("aliases", [])
            )
        )
        if not terms:
            raise ValueError("Entity has no searchable aliases")
        clauses.append(
            "("
            + " OR ".join(
                "(mneme_contains(substr(m.text,1,12000),?) OR mneme_contains(s.title,?) OR mneme_contains(s.directory,?))"
                for _ in terms
            )
            + ")"
        )
        for term in terms:
            values.extend([term, term, term])
    if q.get("mode") in ("concept", "semantic", "hybrid"):
        for aliases in p["required_aliases"].values():
            clauses.append(
                "("
                + " OR ".join(
                    "mneme_contains(substr(m.text,1,12000),?)" for _ in aliases
                )
                + ")"
            )
            values.extend(aliases)
        if p["priority_levels"]:
            patterns = [priority_pattern(level) for level in p["priority_levels"]]
            clauses.append(
                "("
                + " OR ".join(
                    "mneme_pattern(substr(m.text,1,12000),?)" for _ in patterns
                )
                + ")"
            )
            values.extend(patterns)
    for bound, op in zip(p["interval_utc_ms"], [">=", "<"]):
        if bound is not None:
            clauses.append("m.created" + op + "?")
            values.append(bound)
    return clauses, values


def priority_pattern(level):
    return (
        r"\blow\s+(?:prio|priority)"
        if level == "low"
        else r"\b(?:high|top)\s+(?:prio|priority)"
        if level == "high"
        else r"\b" + level + r"\b"
    )


SELECT = "m.key,m.id AS message_id,m.session_key,s.id AS session_id,s.source,s.title,s.directory,m.role,m.created AS created_ms,substr(m.text,1,12000) AS text,length(m.text) AS text_length"
FROM = "messages m JOIN sessions s ON s.key=m.session_key"


def fetch(c, q, p, expression=None, ids=None, chronological=False):
    clauses, values = filters(q, p)
    select, source, order = SELECT, FROM, "m.created DESC,m.key DESC"
    if expression:
        source += " JOIN message_fts ON message_fts.rowid=m.rowid"
        clauses.append("message_fts MATCH ?")
        values.append(expression)
        select += ",bm25(message_fts) AS bm25"
        if not chronological:
            order = "bm25(message_fts),m.key"
    if ids is not None:
        if not ids:
            return []
        clauses.append("m.key IN (" + ",".join("?" for _ in ids) + ")")
        values.extend(ids)
    statement = (
        f"SELECT {select} FROM {source} WHERE "
        + " AND ".join(clauses)
        + f" ORDER BY {order} LIMIT 512"
    )
    return [dict(row) for row in c.execute(statement, values)]


def fts(terms):
    return " OR ".join('"' + t.replace('"', '""') + '"' for t in terms)


def features(p, doc, k, cosine=0.0, lexical_rank=0.0):
    text = doc["text"].lower()
    terms = p["terms"]
    token_coverage = sum(contains(text, t) for t in terms) / max(1, len(terms))
    concepts = {r["id"]: r for r in k["concepts"]}
    coverage = sum(
        any(contains(text, a) for a in concepts[x].get("aliases", []))
        for x in p["concepts"]
    ) / max(1, len(p["concepts"]))
    entity = float(
        any(
            any(
                contains(text, a)
                or contains(doc.get("title") or "", a)
                or contains(doc.get("directory") or "", a)
                for a in e.get("aliases", [])
            )
            for e in p["entities"]
        )
    )
    title = (doc.get("title") or "").lower()
    return [
        float(bool(p["lexical_text"]) and p["lexical_text"].lower() in text),
        token_coverage,
        coverage,
        entity,
        sum(contains(title, t) for t in terms) / max(1, len(terms)),
        max(0.0, float(cosine)),
        lexical_rank,
        min(1.0, math.log1p(len(text)) / 12),
    ]


def vector_candidates(path, q, p):
    import numpy as np

    folder = Path(path).parent / "semantic"
    manifest = json.loads((folder / "manifest.json").read_text())
    data = np.load(folder / manifest["arrays"], allow_pickle=False)
    vocab = manifest["vocabulary"]
    counts = collections.Counter(
        re.findall(r"(?u)\b\w\w+\b", p["lexical_text"].lower())
    )
    columns = [vocab[t] for t in counts if t in vocab]
    weights = np.array(
        [
            (1 + math.log(counts[t])) * data["idf"][vocab[t]]
            for t in counts
            if t in vocab
        ]
    )
    if not columns:
        return {}, manifest
    weights /= max(float(np.linalg.norm(weights)), 1e-12)
    query_vector = weights @ data["components"][:, columns].T
    query_vector /= max(float(np.linalg.norm(query_vector)), 1e-12)
    scores = data["vectors"] @ query_vector
    # Match allowed IDs before top-k: filters must not be applied after truncation.
    c = connection(path)
    clauses, values = filters(q, p)
    allowed = {
        r[0]
        for r in c.execute(
            "SELECT m.key FROM " + FROM + " WHERE " + " AND ".join(clauses), values
        )
    }
    c.close()
    order = sorted(
        (i for i, key in enumerate(manifest["keys"]) if key in allowed),
        key=lambda i: (-float(scores[i]), manifest["keys"][i]),
    )[:512]
    return {
        manifest["keys"][i]: {"score": float(scores[i]), "hash": manifest["hashes"][i]}
        for i in order
        if scores[i] > 0.08
    }, manifest


def search(request):
    q, k = request["query"], knowledge(request)
    p = plan(q, k)
    if q.get("cursor"):
        raise ValueError(
            "Ranked retrieval returns bounded top-k; use native message pagination for the transcript"
        )
    if q.get("harness") not in (None, "opencode"):
        raise ValueError("Only OpenCode is indexed")
    if any(q.get(f) for f in ("tool", "status", "predicate", "asserted_by")):
        raise ValueError(
            "Use history_get_tool_runs or history_get_knowledge for tool/assertion filters"
        )
    if q.get("sort") or q.get("group_by"):
        raise ValueError(
            "Ranked search orders by relevance; use native message tools for chronological sorting and history_aggregate for grouping"
        )
    if not p["terms"] and not p["entities"]:
        raise ValueError("Query needs a meaningful search term or explicit entity")
    mode = q.get("mode") or "concept"
    limit = q.get("limit") or 20
    if not 1 <= limit <= 100:
        raise ValueError("limit must be 1..100")
    c = connection(request["db"])
    generation = c.execute("SELECT value FROM meta WHERE key='generation'").fetchone()[
        0
    ]
    lexical = fetch(
        c,
        q,
        p,
        fts(p["expanded_terms"] if mode in ("concept", "hybrid") else p["terms"]),
    )
    lr = {d["key"]: 1 / (1 + i) for i, d in enumerate(lexical)}
    docs = {d["key"]: d for d in lexical}
    vectors, manifest, warning = {}, None, None
    if mode in ("semantic", "hybrid"):
        try:
            vectors, manifest = vector_candidates(request["db"], q, p)
        except FileNotFoundError:
            if mode == "semantic":
                raise ValueError(
                    "Local LSA model missing; run scripts/build-semantic.py with the same private index"
                )
            warning = "Local LSA model missing; using concept retrieval only"
        semantic_docs = fetch(c, q, p, ids=list(vectors))
        for doc in semantic_docs:
            if (
                hashlib.sha256(doc["text"].encode()).hexdigest()
                == vectors[doc["key"]]["hash"]
            ):
                docs[doc["key"]] = doc
            else:
                vectors.pop(doc["key"], None)
    ranked = []
    concept_order = []
    for key, doc in docs.items():
        definitions = {r["id"]: r for r in k["concepts"]}
        if mode in ("concept", "semantic", "hybrid") and any(
            not any(contains(doc["text"], a) for a in definitions[cid]["aliases"])
            for cid in p["required_mentions"]
        ):
            continue
        if mode in ("concept", "semantic", "hybrid") and p["priority_levels"]:
            patterns = [priority_pattern(level) for level in p["priority_levels"]]
            if not any(re.search(pattern, doc["text"], re.I) for pattern in patterns):
                continue
        f = features(p, doc, k, vectors.get(key, {}).get("score", 0), lr.get(key, 0))
        concept_score = (
            3 * f[0] + 2 * f[1] + 1.2 * f[2] + 2 * f[3] + 0.2 * f[4] + 0.1 * f[6]
        )
        if q.get("entity") and not any(
            any(
                contains(doc["text"], a)
                or contains(doc.get("title") or "", a)
                or contains(doc.get("directory") or "", a)
                for a in e.get("aliases", [])
            )
            for e in p["entities"] + p["related_entities"]
            if e["id"] in p["filter_entity_ids"]
        ):
            continue
        concept_order.append((concept_score, key))
        ranked.append(
            {"key": key, "doc": doc, "features": f, "concept_score": concept_score}
        )
    cr = {
        key: i
        for i, (_, key) in enumerate(sorted(concept_order, key=lambda r: (-r[0], r[1])))
    }
    vr = {
        key: i
        for i, key in enumerate(
            sorted(vectors, key=lambda key: (-vectors[key]["score"], key))
        )
    }
    for row in ranked:
        key = row["key"]
        if mode == "semantic":
            row["score"] = vectors.get(key, {}).get("score", 0)
        elif mode == "ranked_fts":
            row["score"] = lr.get(key, 0)
        elif mode == "hybrid" and manifest:
            row["score"] = (1 / (60 + cr[key]) if key in lr else 0) + (
                1 / (60 + vr[key]) if key in vr else 0
            )
        else:
            row["score"] = row["concept_score"]
    ranked = sorted(
        (r for r in ranked if r["score"] > 0), key=lambda r: (-r["score"], r["key"])
    )
    items, size = [], 0
    for row in ranked[:limit]:
        d = row["doc"]
        matches = [
            m.start()
            for t in p["terms"]
            for m in re.finditer(re.escape(t), d["text"], flags=re.I)
        ]
        offset = (
            q.get("text_offset")
            if q.get("text_offset") is not None
            else max(0, min(matches, default=0) - 150)
        )
        width = q.get("text_limit") or 2000
        if offset < 0 or not 1 <= width <= 16000:
            raise ValueError("Invalid excerpt bounds")
        item = {
            field: d.get(field)
            for field in (
                "source",
                "session_id",
                "message_id",
                "role",
                "created_ms",
                "text_length",
            )
        }
        item.update(
            {
                "harness": "opencode",
                "title": (d.get("title") or "")[:1000],
                "evidence_ref": "mneme:" + row["key"],
                "text": d["text"][offset : offset + width],
                "text_offset": offset,
                "truncated": d["text_length"] > offset + width,
                "retrieval_score": row["score"],
                "retrieval_features": dict(zip(FEATURES, row["features"])),
            }
        )
        encoded = len(json.dumps(item).encode())
        if size + encoded > 64000 and items:
            break
        size += encoded
        items.append(item)
    sources = [
        dict(r) for r in c.execute("SELECT id,version,reconciled,error FROM sources")
    ]
    c.close()
    if manifest:
        manifest = {
            f: manifest[f]
            for f in ("method", "documents", "dimensions", "built_at_ms", "corpus_hash")
        }
    return {
        "items": items,
        "has_more": len(ranked) > len(items),
        "next_cursor": None,
        "generation": generation,
        "plan": p,
        "mode": mode,
        "candidate_cap": 512,
        "sources": sources,
        "semantic_model": manifest,
        "semantic_warning": warning,
        "coverage": "Projected text, at most first 12000 characters per candidate. Vector index is a versioned snapshot; changed vector candidates are excluded. Use lexical search for text beyond this scope.",
        "warning": "Retrieval scores are inferred relevance, not truth, confidence probabilities, successful outcomes or accepted user priorities.",
    }


def run(request):
    tool = request["tool"]
    q, k = request["query"], knowledge(request)
    if tool == "history_explain_query":
        return plan(q, k)
    if tool == "history_get_knowledge":
        p = plan(q, k)
        ids = (
            set(p["filter_entity_ids"])
            if q.get("entity")
            else {e["id"] for e in p["entities"]}
        )
        now = p["as_of_ms"]
        assertions = [
            a
            for a in k.get("assertions", [])
            if (not ids or a["subject"] in ids)
            and (not q.get("predicate") or a["predicate"] == q["predicate"])
            and (not q.get("asserted_by") or a["asserted_by"] == q["asserted_by"])
            and (
                q.get("include_inferred")
                or a["origin"] in ("explicit_user", "project_document")
            )
            and a.get("valid_from_ms", 0) <= now
            and (a.get("valid_until_ms") is None or now < a["valid_until_ms"])
        ]
        limit = q.get("limit") or 20
        return {
            "version": k.get("version"),
            "concepts": [
                x
                for x in k["concepts"]
                if not p["concepts"] or x["id"] in p["concepts"]
            ][:limit],
            "entities": [e for e in k["entities"] if not ids or e["id"] in ids][:limit],
            "assertions": assertions[:limit],
            "candidate_aliases": k.get("candidate_aliases", [])[:limit],
            "has_more": len(assertions) > limit,
            "policy": k.get("policy"),
            "warning": "Absence of a source-backed assertion means unknown, not false. Conflicting assertions are preserved, not silently resolved.",
        }
    return search(request)


if __name__ == "__main__":
    try:
        print(
            json.dumps(run(json.load(sys.stdin)), ensure_ascii=False, allow_nan=False)
        )
    except (ValueError, FileNotFoundError) as error:
        print(json.dumps({"error": str(error)}))
    except Exception:
        print(
            json.dumps(
                {
                    "error": "Local retrieval failed; inspect model availability, index compatibility and numerical dependencies"
                }
            )
        )
