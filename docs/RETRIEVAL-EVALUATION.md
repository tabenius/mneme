# Retrieval quality and learned rerankers — exploratory 0.2 evaluation

## Scope and independence

We inspected a deterministic sample of historical user messages, removed exact
duplicates and excluded the current development conversation, a credential-bearing
message, personal material and an incomplete prompt. The final corpus contains
**33 complete messages from distinct sessions**. There are **36 queries in 12
intent families**, including three unsupported/no-answer queries. All 1188
query/document pairs have an explicit grade: 2 direct evidence, 1 secondary but
useful evidence, 0 unrelated/insufficient evidence. There are 64 positive pairs.

Corpus, query text, grades and manual predictions were frozen before retrieval
scoring, with hashes recorded. Private transcripts, labels and per-document
disagreements are deliberately not committed. The evaluation tools reproduce
the run when pointed at the owner's frozen private fixture.

**These are assistant-authored judgments, not independent human ground truth.**
The sample is small and selected from eligible messages, not representative of
every future query. Recall, sensitivity and specificity are defined only over
this fully judged small corpus, never over the complete archive. Unjudged
full-archive records are not silently treated as irrelevant.

An assistant inspection reference was also recorded before scores: precision
85.5%, recall 100%, nDCG 1.0. It used the same inspected corpus and evaluator as
the labels. It is an explicitly non-independent reference, not evidence that a
model beats retrieval, nor a fair full-history assistant benchmark. The reference
included 13 ultimately judged irrelevant suggestions; guesses are not truth.

## Metrics

Evaluate at most five returned documents:

- Precision = TP/(TP+FP); FDR = FP/(TP+FP).
- Recall/sensitivity = TP/(TP+FN).
- Specificity = TN/(TN+FP), where TN is defined by this small judged universe.
- P@5 = TP/5, with unfilled positions worth zero; distinct from precision among
  returned documents.
- nDCG@5 uses grades 0/1/2 and gain `2^grade-1`; MRR is also recorded.
- Undefined precision/FDR for an empty result and undefined recall for no
  positives are null, not optimistic zeroes. Defined-query counts accompany means.

The table reports macro means; precision/FDR include only queries with returns,
and recall/nDCG include only answerable queries. Per-query confusion matrices and
micro totals remain in the private results artifact. Sparse positives make high
specificity relatively easy: it must not substitute for recall or ranking quality.

## Unsupervised retrieval results

| Method | Precision among returned top-5 | Recall@5 | Specificity | FDR | nDCG@5 |
|---|---:|---:|---:|---:|---:|
| OR/BM25 lexical | .364 | .786 | .921 | .636 | .881 |
| Curated concepts/entities | .396 | .845 | .927 | .604 | .901 |
| Local LSA only | .299 | .816 | .899 | .701 | .874 |
| Concept + LSA hybrid | .323 | .855 | .903 | .677 | .905 |

The original literal-phrase baseline returned nothing for these paraphrased
questions. Original FTS5 (implicit AND) returned results for only seven queries:
precision 1.0, recall@5 .144, nDCG .179. Comparing only against that strict AND
baseline would exaggerate the semantic contribution. OR/BM25 is the important
ablation baseline above.

Concept expansion improves this sample's recall and precision relative to
OR/BM25. Hybrid adds a little more recall/ranking quality but returns more noise.
LSA alone is not the best approach. The evaluation LSA has 16 dimensions on 33
documents; the deployed archive uses 128 dimensions. These sample scores do not
establish quality on the larger archive or predict a modern neural encoder.

A secondary, descriptive top-k sweep illustrates a simpler precision knob:
concept search returning only its first hit had precision **.939** among 33
queries with a result, with recall .662. Across all 36 queries, fixed-slot P@1
was .861; the three unsupported queries abstained. At k=5, recall rises to .845
but precision among returned results falls to .396. Start with `limit=1` for a
targeted lookup and expand when needed rather than treating every returned hit
as equally useful. This sweep was added after the primary k=5 analysis and is
not a preregistered threshold-selection experiment or an independent confidence
calibration.

Descriptive paired query-family bootstrap nDCG deltas vs OR/BM25:

- Concept: +.01995, 95% interval [.00442, .04012].
- Hybrid: +.02431, 95% interval [.00858, .04283].

These intervals resample 11 answerable intent families, not paraphrases as
independent observations. They remain exploratory: same-judge labeling, selection
and possible cross-family correlation limit generalization. No formal confirmatory
significance or full-corpus recall claim is made.

## Would an SVM or tree help?

We tested a linear SVM, shallow classification tree, logistic regression and an
always-negative dummy. Fixed settings, chosen before scores:

- Linear SVM: C=.1, balanced classes, maximum 10000 iterations.
- Logistic regression: C=.1, balanced classes.
- Tree: depth 3, minimum leaf 10, balanced classes.
- Features: literal match, token/concept coverage, explicit entity match, title
  coverage, LSA cosine, lexical rank and log document length. No raw identifiers,
  session IDs or relevance labels are used as features.

Three-fold evaluation separately holds out **session groups** and **query
families**. These are two stress tests, not one simultaneous crossed holdout.
Standardization and classifiers fit only training folds. Unsupervised TF-IDF/LSA
indexes the evaluation document collection without relevance labels, as in normal
transductive retrieval. Hyperparameters were not tuned on held-out results.

Learned rankings preserve the hybrid candidate pool and its attributed-facet
guards. Rank-only rerankers and classifier-threshold filters are evaluated
separately. An initial experiment without this distinction is retained locally;
it is not used to select the release conclusion.

### Rank-only nDCG@5

| Method | Unseen-session protocol | Unseen-query-family protocol |
|---|---:|---:|
| Hybrid baseline | .837 | .905 |
| Logistic reranker | .824 | .907 |
| Linear SVM reranker | .820 | .902 |
| Shallow tree reranker | .755 | .855 |

The protocols have different candidate universes; compare rows **within** a
column, not absolute values across columns. The SVM's unseen-session paired
nDCG delta is -.01536, descriptive interval [-.03780, .00436]. The small tree
generally degrades rankings. Logistic regression's tiny query-family gain is
uncertain and does not survive the session holdout as an improvement.

SVM filtering can trade recall for precision: on held-out query families,
precision rises from .323 to .552 while recall falls from .855 to .817.
Its all-pair classifier sensitivity is .719, specificity .915 and FDR .676;
those classifier metrics have a different denominator than top-5 retrieval.
The always-negative dummy has specificity 1.0 and sensitivity 0, illustrating
why specificity alone is misleading here. No score is a calibrated probability
of statement truth or relevance.

**Decision:** do not promote a learned reranker or hard classifier filter by
default. Preserve explicit concept search and optional hybrid retrieval. A larger
independently reviewed dataset is a better next investment than a more complex
classifier. This does not establish that SVMs/trees can never help.

## Reproduction and next measurement

```sh
python3 -m pip install -r evaluation/requirements.txt
python3 scripts/freeze-evaluation.py --db /private/source.sqlite --fixture /private/eval-v1
python3 scripts/build-semantic.py --db /private/eval-v1/index.sqlite --dimensions 16
python3 scripts/evaluate-retrieval.py --fixture /private/eval-v1 --binary target/debug/mneme
```

The fixture must already contain inspected `candidates.json` and authored
`judgments.json`; it is never fabricated from retrieval output. Freezing refuses
to overwrite an existing experiment. Re-evaluation checks corpus, judgments and
vocabulary hashes and records code hashes. Results and disagreement lists remain
private. Changes to vocabulary or labels need a new versioned experiment.

Next: independently review a stratified set of false discoveries and missed
documents, then expand to more query families and session groups. Include
temporal/entity constraints, ambiguous nouns, negation and explicit-vs-proposed
priorities. Keep a future untouched test split before optimizing thresholds or
evaluating a pretrained local sentence encoder.
