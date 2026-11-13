Step 1: both dirs identical to .agents/skills, neither was a symlink.
Step 2: replaced .claude/skills/gpui and gpui-component with symlinks (120000) to ../../.agents/skills/<name>.
Step 3: refs found: skills-lock.json (skillPath skills/gpui/..., relative to upstream repo, not a local path), docs-site/src/dev/guides/add-gpui-view.md:283, AGENTS.md:226. Not edited.
