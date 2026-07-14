# Synthetic proof datasets

All PoC datasets are generated deterministically from the committed
`manifest-v1.json`. They contain no production data, user data, credentials, or
kernel transcripts.

The manifest is the authority for generator source digest, ordered world IDs,
dataset-specific seed derivation, query judgments, comparator rules, and
workload shape. `calibration-v1` is the only dataset on which policy weights
may be tuned and is the only PR-proof input. `holdout-v1` is frozen and used
only by the deep profile: changing its generator source, derivation domain,
ordered IDs, count, query definitions, or expected rare-event distribution
creates a new dataset version and invalidates prior empirical receipts.

Generated worlds and large scale bodies are build artifacts and are not
committed. The manifest lists every calibration and holdout world ID and
defines its seed as `SHA256(dataset_domain || u64_be(array_position))`. Query
judgments are fixed before execution: episode indices `0`, `1`, and `2`, plus
the single semantic body matching the committed `build-proof` semantic topic,
are binary ordinary-query qrels. Ten one-to-one rare queries judge the
corresponding exact episode at `Recall@1`. These qrels come from the manifest,
never from the candidates returned by a model. A missing judged item remains
in the ideal `NDCG@10` denominator.

A lab proof bundle binds the manifest and generator-source digests, world IDs,
derived seeds, policy, query evidence, actual model bytes, result digests, and
golden fate scenarios. The verifier rejects generator/manifest divergence and
independently replays every committed world, metric, confidence bound, budget,
status, ledger entry, and overall outcome.
