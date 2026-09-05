# Hero animation: explore a workspace with tools, then make a change.
pause 2
say "Explore this project and explain what it does"
wait_reply 120 2
pause 3
say "Add a unit test for parse_temperature"
wait_reply 90 2
# "Review" in the session-changes bar pinned above the composer.
click $(( LOGICAL_W - 117 )) $(( LOGICAL_H - 203 ))
pause 5
