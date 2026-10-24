# Curated knowledge, not automatic truth

`base.json` is the versioned starter vocabulary. It intentionally does not attempt
to collect synonyms for every noun. Related concepts are shown but not expanded
as equivalent words: a project is not its repository, directory, suite or fleet.

Set `MNEME_KNOWLEDGE` to an owner-controlled local JSON file to add private
entities, aliases and assertions. Entries in `concepts`/`entities` replace defaults
with the same ID. Other collections append. Maximum supplemental file size: 1 MiB.
Never commit private paths, statements or session evidence to the starter file.

## Collections

- `concepts`: `id`, `aliases`, `related`, optional `definition` and `attributed`.
- `entities`: `id`, `kind`, `name`, `aliases`, and explicit `relations`, each with
  `predicate`/`object`. Optional source references document who curated a relation.
- `assertions`: `subject`, `predicate`, `value`, `asserted_by`, `origin`,
  `source_ref`, optional `asserted_at_ms`, `valid_from_ms`, `valid_until_ms`.
- `candidate_aliases`: unreviewed suggestions. These are visible but never activated
  by query expansion. An operator reviews context and edits the curated file.

Supported assertion origins: `explicit_user`, `project_document`,
`assistant_proposal`, `inference`. The last two are excluded unless
`include_inferred=true`. Source references are mandatory pointers supplied by the
operator; they are not independently verified attestations. Conflicting active
assertions are preserved, not silently resolved. Missing assertions mean unknown.

For example, a synthetic operator file could contain:

```json
{
  "assertions": [{
    "subject": "project:example",
    "predicate": "priority",
    "value": "p1",
    "asserted_by": "user",
    "origin": "explicit_user",
    "source_ref": "synthetic:message-1",
    "valid_from_ms": 1000,
    "valid_until_ms": null
  }]
}
```

Priority, urgency, preference, value and flagship designation are separate
properties. Querying an attributed facet requires an actual lexical mention of
that facet, not merely a vector-near project. Priority levels are not interchangeable.

One-hop entity traversal follows explicit `has_repository`, `located_at`,
`implements` links and reverse suite `belongs_to` membership. It is explained
as related entities, not a claim that distinct things are synonyms. Repository
IDs remain owned by Frog; Mneme does not invent a competing repository identity.
