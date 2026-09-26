with open('CLAUDE.md', 'r') as f:
    content = f.read()

marker_head = "<<<<<<< HEAD\n"
marker_mid = "=======\n"
marker_main = ">>>>>>> origin/main\n"

start = content.index(marker_head)
mid = content.index(marker_mid, start)
end = content.index(marker_main, mid)
end_line_end = content.index("\n", end) + 1

head_text = content[start+len(marker_head):mid]
main_text = content[mid+len(marker_mid):end]

assert head_text.endswith("shell has since restarted.\n"), repr(head_text[-50:])
assert main_text.startswith(head_text[:200])
assert "T9 (AGE-587)" in main_text
assert "only records the level today" in main_text

t9_marker = " T9 (AGE-587)"
idx = main_text.index(t9_marker)
t9_text = main_text[idx:]

merged_text = head_text.rstrip("\n") + t9_text

new_content = content[:start] + merged_text + content[end_line_end:]

with open('CLAUDE.md', 'w') as f:
    f.write(new_content)

print("done, merged_text length:", len(merged_text))
