# Release Process

1. Update version in `Cargo.toml`
2. Commit: `git commit -am "Bump version to X.Y.Z"`
3. Tag: `git tag vX.Y.Z`
4. Push: `git push origin main --tags`
5. GitHub Actions will automatically build and create the release