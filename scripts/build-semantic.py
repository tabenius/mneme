#!/usr/bin/env python3
"""Build a local LSA snapshot. Never downloads models or transmits transcripts.

Dependencies: numpy, scipy, scikit-learn. No pickle/joblib artifacts are loaded.
"""

import os

os.environ.setdefault("OPENBLAS_NUM_THREADS", "2")
os.environ.setdefault("OMP_NUM_THREADS", "2")
import argparse
import fcntl
import hashlib
import json
import sqlite3
import time
from pathlib import Path

import numpy as np
from sklearn.decomposition import TruncatedSVD
from sklearn.feature_extraction.text import TfidfVectorizer
from sklearn.preprocessing import normalize

parser = argparse.ArgumentParser()
parser.add_argument("--db", required=True)
parser.add_argument("--dimensions", type=int, default=128)
args = parser.parse_args()
assert 2 <= args.dimensions <= 512
os.umask(0o077)
path = Path(args.db).resolve()
folder = path.parent / "semantic"
folder.mkdir(mode=0o700, exist_ok=True)
lock = (folder / "build.lock").open("a")
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
c = sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)
c.execute("BEGIN")
rows = c.execute(
    "SELECT m.key,substr(m.text,1,12000) FROM messages m JOIN sessions s ON s.key=m.session_key WHERE s.deleted=0 AND length(m.text)>1 ORDER BY m.key"
).fetchall()
c.close()
if len(rows) < 4:
    raise SystemExit("Need at least four nonempty messages")
keys, texts = zip(*rows)
hashes = [hashlib.sha256(t.encode()).hexdigest() for t in texts]
vectorizer = TfidfVectorizer(
    max_features=30000, min_df=1, sublinear_tf=True, dtype=np.float32
)
x = vectorizer.fit_transform(texts)
dimensions = min(args.dimensions, x.shape[1] - 1, x.shape[0] - 1)
if dimensions < 2:
    raise SystemExit("Insufficient vocabulary for LSA")
svd = TruncatedSVD(n_components=dimensions, n_iter=5, random_state=20261024)
vectors = normalize(svd.fit_transform(x)).astype(np.float32)
digest = hashlib.sha256(json.dumps(list(zip(keys, hashes))).encode()).hexdigest()
previous = {}
if (folder / "manifest.json").exists():
    previous = json.loads((folder / "manifest.json").read_text())
filename = "lsa-" + digest[:16] + "-" + str(dimensions) + ".npz"
temporary = folder / (filename + ".tmp")
with temporary.open("wb") as stream:
    np.savez(
        stream,
        vectors=vectors,
        components=svd.components_.astype(np.float32),
        idf=vectorizer.idf_.astype(np.float32),
    )
os.replace(temporary, folder / filename)
manifest = {
    "method": "local-tfidf-lsa",
    "version": 1,
    "documents": len(rows),
    "dimensions": dimensions,
    "built_at_ms": int(time.time() * 1000),
    "corpus_hash": digest,
    "arrays": filename,
    "keys": keys,
    "hashes": hashes,
    "vocabulary": vectorizer.vocabulary_,
    "scope": "First 12000 characters of projected message text. Unsupervised transductive index, not a pretrained neural encoder.",
    "managed_arrays": list(
        dict.fromkeys(
            ([previous["arrays"]] if previous.get("arrays") else []) + [filename]
        )
    ),
}
temporary = folder / "manifest.tmp"
temporary.write_text(json.dumps(manifest, default=int))
os.replace(temporary, folder / "manifest.json")
# Prune only files explicitly recorded by our previous manifest, retaining one rollback generation.
import re

for old in previous.get("managed_arrays", []):
    candidate = folder / old
    if (
        old not in manifest["managed_arrays"]
        and re.fullmatch(r"lsa-[0-9a-f]{16}(?:-[0-9]+)?\.npz", old)
        and not candidate.is_symlink()
    ):
        candidate.unlink(missing_ok=True)
print(
    json.dumps(
        {
            key: manifest[key]
            for key in [
                "method",
                "documents",
                "dimensions",
                "built_at_ms",
                "corpus_hash",
            ]
        }
    )
)
