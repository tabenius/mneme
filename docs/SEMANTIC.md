# Semantic retrieval milestones (0.2)

All six initial implementation milestones are available, as bounded foundations:

1. **Curated vocabulary:** versioned aliases, distinct related concepts, operator-reviewed candidates.
2. **Entity resolution:** explicit project/repository/directory/suite identities, aliases and one-hop relations.
3. **Time interpretation:** Stockholm calendar intervals, UTC millisecond bounds and a frozen `as_of_ms` for tests.
4. **Attributed assertions:** origin/actor/source/validity, proposals excluded by default, conflicts retained.
5. **Concept-enriched lexical search:** explained expansions, lexical/term/concept/entity features and guards for attributed facets.
6. **Local vector/hybrid retrieval:** TF-IDF + LSA cosine retrieval and reciprocal-rank fusion. No external inference or model downloads.

This is not an unrestricted natural-language query interpreter, complete ontology,
automatic assertion extractor or pretrained neural sentence encoder. These are
small testable layers, not claims that semantic search is solved.

## Search modes

- `literal`: original exact substring search (unchanged default).
- `fts`: original SQLite FTS5 syntax (unchanged).
- `ranked_fts`: natural-language terms combined with OR, ranked by BM25. This is
  the fair lexical baseline for expanded natural-language queries.
- `concept`: curated aliases plus explicit entities; sensible first choice for a
  natural-language historical lookup.
- `semantic`: local LSA distributional vectors; experimental and sometimes noisy.
- `hybrid`: reciprocal-rank fusion of concept and LSA results; greater recall
  is not automatically greater precision.

```sh
mneme query history_explain_query --args '{"query":"repos last week"}'
mneme query history_search --args '{"query":"repository locations","mode":"concept"}'
mneme query history_search --args '{"query":"document preparation","mode":"hybrid"}'
mneme query history_get_knowledge --args '{"entity":"project:mneme","predicate":"priority"}'
```

Ranked search returns bounded top-k with feature scores and interpretation; it
does not offer transcript cursors or accept chronological sorts. Retrieve a
message/transcript with the existing native pagination tools. Candidates are
capped at 512 per lexical/vector source; scoring examines the first 12000 text
characters. Explicit filters are applied before candidate truncation. Entity
filters use declared identities/relations, not guessed directory containment.
Vector candidates whose content changed are excluded. Native lexical retrieval
remains available for material outside the vector snapshot/text scope.

Calendar `last_week` means previous Monday through Monday in Europe/Stockholm;
it can contain 167 or 169 hours at DST transitions. `past_7_days` means 168 hours.
`today`, `yesterday`, `this_week` and `past_30_days` are also supported. Explicit
time bounds intersect interpreted bounds. Meaningless/empty intervals are rejected.

## Local model lifecycle

```sh
python3 -m pip install -r evaluation/requirements.txt  # preferably in an isolated environment
mneme --db /private/index.sqlite semantic-build --dimensions 128
```

The builder reads a consistent SQLite snapshot of nonempty projected message
texts, computes TF-IDF and truncated SVD, then atomically publishes NumPy arrays
and a manifest. It uses at most two numerical-library threads by default and
does not download an encoder. It never loads pickle/joblib files. Arrays and
vocabulary are sensitive local artifacts, not public model weights.

The manifest identifies keys, text hashes, dimension, generation data and build
time. Only the current and previous recorded array generations are retained;
cleanup touches only files recorded in our own previous manifest. A build lock
prevents concurrent writers. This is snapshot refresh, not incremental SVD.

An optional headless `mneme-semantic-refresh.timer` refreshes hourly. Its service
uses the same private environment file as the collector. Set `MNEME_PYTHON` when
numerical packages live in a dedicated interpreter. Literal/FTS search and the
collector do not require those packages. A failed model build does not stop them.

`history_index_status` exposes model metadata without returning vocabulary or
transcripts. The model snapshot can be older than current text. Unknown or
unretained source events remain unknown; embedding text does not recover them.

## Scientific caution

Retrieval relevance is not statement truth, inference confidence, commercial
value or proof of execution. Scores are not calibrated probabilities. A recorded
priority is attributed to an actor and time; a frequently mentioned project is
not automatically a favorite, flagship or urgent task.

See [retrieval evaluation](RETRIEVAL-EVALUATION.md) for measured precision, recall,
specificity, FDR, ranking quality and the SVM/tree experiment.

## Primary references

- [scikit-learn LSA](https://scikit-learn.org/stable/modules/decomposition.html#truncated-singular-value-decomposition-and-latent-semantic-analysis)
- [SVM](https://scikit-learn.org/stable/modules/svm.html): margins are not probabilities; regularization matters.
- [Decision trees](https://scikit-learn.org/stable/modules/tree.html): constrain depth/leaves to limit overfitting and instability.
