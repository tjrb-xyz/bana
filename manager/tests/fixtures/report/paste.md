# CI report: tjrb-xyz/example · failed

A pasted log

| Standard | Checks | Tests | Not run here |
|---|---|---|---|
| rust | 0% (0/1) | 95% of 22 run (incomplete) | |
| **all** | 0% (0/1) | 95% of 22 run (incomplete) | |

## Failures

**rust › cargo test --workspace**

- `real_c3_the_engine_accepts_only_its_token_and_no_origin` at crates/example-engine/tests/facts.rs:457:18: `accepted`
- Rerun: `cargo test -p example-engine --test facts`
- Incomplete: cargo stopped at the first failing test binary (add `--no-fail-fast`).

```
thread 'real_c3_the_engine_accepts_only_its_token_and_no_origin' (10194) panicked at crates/example-engine/tests/facts.rs:457:18:
accepted
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    real_c3_the_engine_accepts_only_its_token_and_no_origin

test result: FAILED. 21 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.84s

error: test failed, to rerun pass `-p example-engine --test facts`
```

## Not the project's

- bana: `Error occurred running finally: symlink log-only /Users/lilly/.cache/act/68f407a87fd03002/act/actions/tjrb-xyz-bana-actions-keep-builds@a4b6f87212d190304c530041b9bbd5fed72f0dd3/tests/stand-ins/apt-get: file exists`
