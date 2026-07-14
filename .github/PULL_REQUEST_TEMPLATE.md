## Claim and scope

<!-- What single claim or behavior changes? State explicit non-claims. -->

## Policy and compatibility

- Policy ID and digest:
- Schema/canonical encoding impact:
- Migration or rollback impact:

## Deterministic inputs

- Explicit `as_of_us`:
- Seed(s) and purpose:
- Dataset version(s):

## Proof evidence

- Receipt path or CI artifact:
- Receipt digest:
- Replay result:
- Invariant/hypothesis status:

## Security and data

- [ ] Fixtures and evidence contain synthetic, non-sensitive data only.
- [ ] Memory-space isolation, artifact closure, and tamper behavior were considered.
- [ ] New dependencies or workflow permissions were reviewed.

## Tests

<!-- List exact local commands and results. Include golden/property/differential/crash/scale evidence where applicable. -->

## Checklist

- [ ] Every commit has a DCO `Signed-off-by` line.
- [ ] The change is one independently verifiable slice.
- [ ] Documentation, schemas, golden fixtures, and policy are synchronized.
- [ ] No hidden clock, RNG, float, host path, unordered iteration, LLM, or embedding affects logical output.
- [ ] I did not tune policy against a previously observed `holdout-v1` result.
