# CI report: failed

6s

| Standard | Checks | Tests | Not run here |
|---|---|---|---|
| matrix (x64) | 100% (2/2) | — | |
| matrix (arm64) | 100% (2/2) | — | |
| host | 40% (2/5) | 81% (9/11), 2 skipped | |
| node | 83% (5/6) | 50% (1/2), 2 skipped | |
| py | 100% (1/1) | 100% (1/1), 1 skipped | |
| consume | 100% (4/4) | — | |
| **all** | 80% (16/20) | 78% (11/14), 5 skipped | |

## Failures

**host › cargo test**

- `tests::accepted` at src/lib.rs:12:45: `accepted`
- Rerun: `cargo test --lib`

```
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests demo

running 1 test
test src/lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

all doctests ran in 0.35s; merged doctests compilation took 0.34s
error: 1 target failed:
    `--lib`
```

**host › nextest**

- `tests::accepted` at src/lib.rs:12:45: `accepted`

```
    thread 'tests::accepted' (10859) panicked at src/lib.rs:12:45:
    accepted
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

        PASS [   0.012s] (3/5) demo::facts fact_one
        PASS [   0.018s] (4/5) demo tests::adds
        PASS [   0.008s] (5/5) demo::facts real::raw_file_end_stops_with_done
------------
     Summary [   0.020s] 5 tests run: 4 passed, 1 failed, 1 skipped
        FAIL [   0.009s] (2/5) demo tests::accepted
error: test run failed
```

**host › actions/upload-artifact@v4**

- `request blocked: no rule allows host "192.0.2.2"`

```
(node:10981) [DEP0040] DeprecationWarning: The `punycode` module is deprecated. Please use a userland alternative instead.
(Use `node --trace-deprecation ...` to show where the warning was created)
With the provided path, there will be 1 file uploaded
Artifact name is valid!
Root directory input is valid!
Beginning upload of artifact content to blob storage
❗  ::error::request blocked: no rule allows host "192.0.2.2"
```

**node › node tests**

- node: 1 failed

```
      at Test.run (node:internal/test_runner/test:1382:25)
      at Test.processPendingSubtests (node:internal/test_runner/test:960:18)
      at Test.postRun (node:internal/test_runner/test:1522:19)
      at Test.run (node:internal/test_runner/test:1447:12)
      at async startSubtestAfterBootstrap (node:internal/test_runner/harness:387:3) {
    generatedMessage: false,
    code: 'ERR_ASSERTION',
    actual: false,
    expected: true,
    operator: '==',
    diff: 'simple'
  }
```

## Step summaries

**host › summary from host mode**

host summary

**node › summary one**

## node job
| a | b |
|---|---|
| 1 | 2 |

**node › summary two**

second step summary
