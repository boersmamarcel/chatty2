# AGE-547 plan

## Acceptance criteria
- Symlink escape refused under local_only and open policies
- Real files, inward symlinks and non-existing paths still allowed
- Existing profile.rs tests pass unchanged
- clippy -D warnings and fmt pass

## Steps
1. [ ] Add `resolve()` in profile.rs
2. [ ] `path_is_within` uses `resolve`; doc comment updated
3. [ ] Test a_symlink_out_of_the_workspace_is_refused
4. [ ] Test real_files_and_inward_symlinks_stay_allowed
