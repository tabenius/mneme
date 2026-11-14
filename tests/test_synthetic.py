"""Synthetic corpus invariants: never reads the owner's session history."""

import importlib.util
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "synthetic_corpus", ROOT / "scripts/synthetic-corpus.py"
)
corpus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(corpus)


class SyntheticCorpusTests(unittest.TestCase):
    def setUp(self):
        self.docs, self.queries, self.sessions = corpus.build()

    def test_generation_is_deterministic(self):
        again = corpus.build()
        self.assertEqual(
            corpus.corpus_hash(self.docs, self.queries),
            corpus.corpus_hash(again[0], again[1]),
        )
        self.assertEqual(len(self.docs), len(again[0]))
        self.assertEqual(len(self.sessions), len(again[2]))

    def test_every_document_belongs_to_a_declared_session(self):
        keys = {doc["key"] for doc in self.docs}
        session_keys = {doc["session_key"] for doc in self.docs}
        self.assertEqual(len(keys), len(self.docs))
        self.assertEqual(session_keys, {session["key"] for session in self.sessions})
        self.assertEqual(
            len({doc["doc_id"] for doc in self.docs}), len(self.docs)
        )

    def test_judgments_are_complete_and_constructed(self):
        ids = {doc["doc_id"] for doc in self.docs}
        by_id = {doc["doc_id"]: doc for doc in self.docs}
        topics = {query["family"] for query in self.queries} - {"unsupported"}
        self.assertEqual(topics, set(corpus.TOPICS))
        for query in self.queries:
            self.assertTrue(set(query["relevant"]) <= ids, query["id"])
            if query["family"] == "unsupported":
                self.assertEqual(query["relevant"], {})
                continue
            self.assertTrue(query["relevant"], query["id"])
            for doc_id, grade in query["relevant"].items():
                self.assertEqual(grade, 2)
                self.assertEqual(by_id[doc_id]["topic"], query["family"])
                if not query["id"].endswith("_exact"):
                    self.assertIn(by_id[doc_id]["entity"], query["query"])
                else:
                    self.assertIn(query["query"].lower(), by_id[doc_id]["text"].lower())

    def test_schema_is_parsed_from_the_authoritative_source(self):
        schema = corpus.schema_sql()
        for fragment in [
            "CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5",
            "CREATE TRIGGER IF NOT EXISTS messages_ai",
            "PRAGMA user_version=2;",
        ]:
            self.assertIn(fragment, schema)
        database = sqlite3.connect(":memory:")
        database.executescript(schema)
        version = database.execute("PRAGMA user_version").fetchone()[0]
        self.assertEqual(version, 2)
        tables = {
            row[0]
            for row in database.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"
            )
        }
        self.assertIn("messages", tables)
        self.assertIn("message_fts", tables)

    def test_written_index_populates_fts_through_triggers(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "index.sqlite"
            corpus.write_index(path, self.docs, self.sessions)
            database = sqlite3.connect(path)
            try:
                messages = database.execute("SELECT count(*) FROM messages").fetchone()[
                    0
                ]
                hits = database.execute(
                    "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'repository'"
                ).fetchone()[0]
                generation = database.execute(
                    "SELECT value FROM meta WHERE key='generation'"
                ).fetchone()[0]
            finally:
                database.close()
            self.assertEqual(messages, len(self.docs))
            self.assertGreater(hits, 0)
            self.assertEqual(generation, 1)
            self.assertEqual(path.read_bytes()[:15], b"SQLite format 3")


if __name__ == "__main__":
    unittest.main()
