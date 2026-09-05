# The PR bar resolves as soon as the window opens: the workspace is a git
# checkout whose origin is on GitHub (see setup.sh), so the bar above the
# composer shows the pull request for the current branch.
pause 2
collapse_sidebar
pause 2.5
say "What is on this branch?"
wait_reply 90 2.5
