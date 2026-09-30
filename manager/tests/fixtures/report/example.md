# CI report: tjrb-xyz/example · speaker-check d4b5174 · quick · failed

Build #42 on mbp · act 0.2.89 · network host · bana a4b6f87 · 12m 40s

| Standard | Checks | Tests | Not run here |
|---|---|---|---|
| toolchain | 100% (2/2) | — | |
| rust | 0% (0/1) | 95% of 22 run (incomplete) | |
| engine | — | 90% of 10 run (incomplete) | |
| web | 100% (2/2) | 100% (32/32), 2 skipped | |
| macos | 100% (2/2) | — | macos › On a dedicated CI Mac only (left out) |
| streaming | 100% (1/1) | 100% (18/18) | |
| packaging | — | — | package (linux-arm64) (not planned at quick), package (macos-arm64) (no platform for it here) |
| **all** | 90% (10/11) | 98% of 72 run (incomplete), 2 skipped | 3 |

## Failures

**rust › cargo test --workspace**

- `real_c3_the_engine_accepts_only_its_token_and_no_origin` at crates/example-engine/tests/facts.rs:457:18: `accepted`
- Rerun: `cargo test -p example-engine --test facts`
- Incomplete: cargo stopped at the first failing test binary (add `--no-fail-fast`).

```
test real_c3_the_engine_accepts_only_its_token_and_no_origin ... FAILED

failures:

---- real_c3_the_engine_accepts_only_its_token_and_no_origin stdout ----

thread 'real_c3_the_engine_accepts_only_its_token_and_no_origin' (4711) panicked at crates/example-engine/tests/facts.rs:457:18:
accepted

test result: FAILED. 21 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.84s

error: test failed, to rerun pass `-p example-engine --test facts`
```

## Not the project's

- bana: macos › Post tjrb-xyz/bana/actions/keep-builds@a4b6f87: `symlink log-only /Users/lilly/.cache/act/68f407a87fd03002/act/actions/tjrb-xyz-bana-actions-keep-builds@a4b6f87212d190304c530041b9bbd5fed72f0dd3/tests/stand-ins/apt-get: file exists`

## Not run here

- macos › On a dedicated CI Mac only: `Not a dedicated CI Mac: the driver's install is left out`
- package (linux-arm64): not planned at quick
- package (macos-arm64): no platform for it here

## Step summaries

**web › pnpm test**

## Vitest Test Report

| Passed | Failed | Skipped |
|---|---|---|
| 32 | 0 | 2 |
