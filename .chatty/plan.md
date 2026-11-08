# Plan: AGE-920 clear error for a too-long socket path

## Acceptance criteria
- 3 new tests pass (short path accepted, 150-byte path refused with length, bind in too-long dir fails clearly)
- existing `access::` tests pass
- clippy -D warnings and fmt pass
- only `crates/chatty-protocol-gateway/src/access.rs` changes (besides .chatty/ state)

## Steps
1. [x] Add SOCKET_PATH_MAX + check_socket_path_len, use in default_runtime_dir and SocketServer::bind, add tests — commit: see git log (AGE-920 commit)
