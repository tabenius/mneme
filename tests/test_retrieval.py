"""Synthetic tests: never reads the owner's session history."""

import copy
import datetime as dt
import importlib.util
import json
import unittest
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "retrieval", ROOT / "scripts/retrieval.py"
)
engine = importlib.util.module_from_spec(spec)
spec.loader.exec_module(engine)
spec = importlib.util.spec_from_file_location(
    "evaluate", ROOT / "scripts/evaluate-retrieval.py"
)
evaluate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluate)


class RetrievalTests(unittest.TestCase):
    def setUp(self):
        self.knowledge = json.loads((ROOT / "knowledge/base.json").read_text())

    def test_synonyms_not_related_concepts(self):
        p = engine.plan({"query": "repo"}, self.knowledge)
        self.assertIn("repository", p["expanded_terms"])
        self.assertNotIn("directory", p["expanded_terms"])

    def test_entity_alias_resolution(self):
        p = engine.plan({"query": "ragbaz-mneme"}, self.knowledge)
        self.assertIn("project:mneme", [e["id"] for e in p["entities"]])
        with self.assertRaises(ValueError):
            engine.plan({"entity": "not-registered"}, self.knowledge)
        with self.assertRaises(ValueError):
            engine.plan({"entity": "not-registered", "query": "frog"}, self.knowledge)
        mixed = engine.plan(
            {"entity": "project:mneme", "query": "frog"}, self.knowledge
        )
        self.assertEqual(mixed["filter_entity_ids"], ["project:mneme"])

    def test_suite_membership_uses_only_explicit_links(self):
        p = engine.plan(
            {"entity": "suite:ragbaz", "query": "repositories"}, self.knowledge
        )
        self.assertIn("project:mneme", p["filter_entity_ids"])
        self.assertIn("nostoi", p["expanded_terms"])
        self.assertNotIn("project:unknown", p["filter_entity_ids"])

    def test_temporal_dst_previous_week_not_rolling_duration(self):
        as_of = int(
            dt.datetime.fromisoformat("2026-10-26T12:00:00+01:00").timestamp() * 1000
        )
        start, end = engine.interval("last_week", as_of)
        self.assertEqual(end - start, 169 * 3600 * 1000)
        start, end = engine.interval("past_7_days", as_of)
        self.assertEqual(end - start, 168 * 3600 * 1000)

    def test_temporal_phrase_removed_and_explicit_bounds_intersect(self):
        as_of = 1793000000000
        p = engine.plan({"query": "repo last week", "as_of_ms": as_of}, self.knowledge)
        self.assertNotIn("week", p["terms"])
        q = engine.plan(
            {
                "query": "repo",
                "time_frame": "past_7_days",
                "as_of_ms": as_of,
                "since_ms": as_of - 1000,
            },
            self.knowledge,
        )
        self.assertEqual(q["interval_utc_ms"], [as_of - 1000, as_of])

    def test_priority_urgency_and_preference_are_distinct(self):
        p = engine.plan({"query": "low priority favorite project"}, self.knowledge)
        self.assertEqual(p["priority_level"], "low")
        self.assertIn("preference", p["required_mentions"])
        self.assertNotIn("urgency", p["required_mentions"])
        both = engine.plan({"query": "p0 p1 priorities"}, self.knowledge)
        self.assertEqual(both["priority_levels"], ["p0", "p1"])

    def test_assertions_require_provenance(self):
        self.knowledge["assertions"] = [
            {"subject": "project:mneme", "predicate": "priority"}
        ]
        with self.assertRaises(ValueError):
            engine.knowledge({"knowledge": self.knowledge})

    def test_unknown_is_not_false_and_proposals_excluded(self):
        base = {
            "subject": "project:mneme",
            "predicate": "flagship",
            "value": True,
            "asserted_by": "assistant",
            "source_ref": "synthetic:1",
            "origin": "assistant_proposal",
        }
        self.knowledge["assertions"] = [base]
        req = {
            "tool": "history_get_knowledge",
            "query": {"entity": "project:mneme", "predicate": "flagship"},
            "knowledge": self.knowledge,
        }
        self.assertEqual(engine.run(copy.deepcopy(req))["assertions"], [])
        req["query"]["include_inferred"] = True
        self.assertEqual(len(engine.run(req)["assertions"]), 1)

    def test_conflicts_and_temporal_validity_are_preserved(self):
        base = {
            "subject": "project:mneme",
            "predicate": "priority",
            "asserted_by": "user",
            "source_ref": "synthetic:1",
            "origin": "explicit_user",
            "valid_from_ms": 10,
            "valid_until_ms": 20,
        }
        self.knowledge["assertions"] = [dict(base, value="p0"), dict(base, value="p1")]
        req = {
            "tool": "history_get_knowledge",
            "query": {"as_of_ms": 15},
            "knowledge": self.knowledge,
        }
        self.assertEqual(len(engine.run(copy.deepcopy(req))["assertions"]), 2)
        req["query"]["as_of_ms"] = 20
        self.assertEqual(engine.run(req)["assertions"], [])

    def test_precision_recall_specificity_fdr_denominators(self):
        m = evaluate.metrics(["a", "c"], {"a": 2, "b": 1}, {"a", "b", "c", "d"})
        for field in ["precision", "recall_at_5", "specificity", "fdr"]:
            self.assertEqual(m[field], 0.5)
        self.assertEqual(m["precision_at_5"], 0.2)

    def test_undefined_metrics_are_not_optimistic_zeros(self):
        m = evaluate.metrics([], {}, set("abcd"))
        self.assertIsNone(m["precision"])
        self.assertIsNone(m["recall_at_5"])
        self.assertIsNone(m["fdr"])
        self.assertEqual(m["specificity"], 1)

    def test_real_local_lsa_encoder_and_stale_vector_exclusion(self):
        with tempfile.TemporaryDirectory() as directory:
            db = Path(directory) / "index.sqlite"
            c = sqlite3.connect(db)
            c.executescript(
                "CREATE TABLE meta(key TEXT,value INTEGER); INSERT INTO meta VALUES('generation',1); CREATE TABLE sources(id TEXT,version TEXT,reconciled INTEGER,error TEXT); INSERT INTO sources VALUES('fixture','2.test',0,NULL); CREATE TABLE sessions(key TEXT,id TEXT,source TEXT,title TEXT,directory TEXT,deleted INTEGER,parent TEXT,project TEXT,agent TEXT,model TEXT); INSERT INTO sessions VALUES('s','ses_fixture','fixture','Synthetic','/fixture',0,NULL,'p','build','test'); CREATE TABLE messages(key TEXT,id TEXT,session_key TEXT,role TEXT,created INTEGER,text TEXT); CREATE VIRTUAL TABLE message_fts USING fts5(text);"
            )
            texts = [
                "quartz minerals crystallography",
                "oranges citrus orchard",
                "cobalt battery electrode",
                "volcanic basalt mountain",
            ]
            for i, text in enumerate(texts):
                c.execute(
                    "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
                    (f"k{i}", f"msg_{i}", "s", i, text),
                )
                c.execute("INSERT INTO message_fts(text) VALUES(?)", (text,))
            c.commit()
            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts/build-semantic.py"),
                    "--db",
                    str(db),
                    "--dimensions",
                    "3",
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            req = {
                "db": str(db),
                "tool": "history_search",
                "query": {"query": texts[0], "mode": "semantic"},
                "knowledge": self.knowledge,
            }
            value = engine.run(copy.deepcopy(req))
            self.assertEqual(value["items"][0]["message_id"], "msg_0")
            self.assertGreater(value["items"][0]["retrieval_score"], 0.999)
            c.execute(
                "UPDATE messages SET text='changed since model build' WHERE key='k0'"
            )
            c.commit()
            value = engine.run(req)
            self.assertNotIn("msg_0", [r["message_id"] for r in value["items"]])
            c.close()


if __name__ == "__main__":
    unittest.main()
