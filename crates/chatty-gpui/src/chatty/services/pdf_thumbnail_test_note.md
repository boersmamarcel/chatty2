# PDF Thumbnail Tests

## Running Tests

The PDF thumbnail tests use a shared global thumbnail directory. To avoid race conditions when tests clean up this shared resource, run these tests serially:

```bash
# Run all PDF thumbnail tests serially
cargo test chatty::services::pdf_thumbnail::tests -- --test-threads=1

# Or run all tests serially
cargo test -- --test-threads=1
```

When running tests in parallel (default), there may be occasional race conditions where one test's cleanup interferes with another test's file operations.

## Test Coverage

The test suite covers:

1. **Valid PDF rendering** - Confirms thumbnails are generated correctly for valid PDFs
2. **Invalid PDF handling** - Ensures errors are returned for malformed PDFs  
3. **Missing file handling** - Validates error handling for non-existent files
4. **Thumbnail directory creation** - Tests session temp directory management
5. **Cleanup functionality** - Verifies proper cleanup of temp directories
6. **Multiple thumbnails** - Tests concurrent thumbnail generation
7. **Idempotency** - Ensures same PDF produces same thumbnail path
