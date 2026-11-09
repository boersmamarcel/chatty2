# AGE-1093 plan

## Acceptance criteria
- A symlinked or non-directory `daytona_outputs` stops the download; nothing is written.
- A missing dir is created; an existing dir is used.
- 4 new tests and existing daytona_tool tests pass; clippy -D warnings and fmt pass.

## Steps
1. [x] Add `prepare_download_dir`, use it in `download_generated_files`, add 4 tests (commit: see git log, "AGE-1093")
