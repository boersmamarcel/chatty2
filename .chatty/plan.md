# Plan: AGE-920 socket path length check

## Acceptance criteria
- Only crates/chatty-protocol-gateway/src/access.rs changes (besides .chatty files, removed later).
- `pub const SOCKET_PATH_MAX: usize = 104;` below DIR_NAME, documented.
- `pub fn check_socket_path_len(&Path) -> io::Result<()>`: Ok when len < 104, else InvalidInput naming path, length, limit, suggesting a short XDG_RUNTIME_DIR.
- Tests `a_short_socket_path_is_accepted` and `a_socket_path_over_the_limit_is_refused_with_its_length`.
- check_socket_path_len is not called anywhere.

## Steps
1. [x] Bring over const + fn + 2 tests only; diff vs origin/main touches only access.rs (commit 6f51aec6)
